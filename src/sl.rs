//! Read-only Sapling access: scopes, changed files, and the snapshot store.
//!
//! See `specs/sapling.md`. Every `sl` call here only reads. reviewr writes nothing
//! inside a Sapling repository (SL-NO-REPO-WRITES): the turn baseline and the base
//! pick live in the snapshot store, a directory under reviewr's own state dir. Every
//! command runs with `HGPLAIN=1`, with the repository root as its working directory,
//! and parses stdout only — stderr carries watchman warnings that would corrupt a
//! merged-stream parse.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::git::{BaseStatus, GitFail, ResolvedBase, Worktree};
use crate::model::{ChangeKind, ChangedFile, Scope};

/// Run `sl <args>` at `root` and return its raw output.
fn sl_out(root: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    crate::proc::command("sl").current_dir(root).env("HGPLAIN", "1").args(args).output()
}

/// Run `sl <args>` and return stdout. Errors on non-zero exit.
fn sl(root: &Path, args: &[&str]) -> Result<String> {
    let out = sl_out(root, args).with_context(|| format!("running sl {args:?}"))?;
    if !out.status.success() {
        bail!("sl {args:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Resolve `path` to its Sapling repository root, keeping "sl said no" (`Outside`)
/// apart from "sl could not run" (`Unknown`) — the same tri-state contract as the git
/// resolver (`specs/herdr-host.md`).
pub fn root_of(path: &Path) -> Worktree {
    // A directory that does not exist cannot be a worktree; without this check the
    // spawn's cwd error would read as `Unknown` and hold the sample forever.
    if !path.is_dir() {
        return Worktree::Outside;
    }
    match sl_out(path, &["root"]) {
        Err(_) => Worktree::Unknown,
        Ok(out) if !out.status.success() => Worktree::Outside,
        Ok(out) => match String::from_utf8_lossy(&out.stdout).trim() {
            "" => Worktree::Outside,
            root => Worktree::Root(PathBuf::from(root)),
        },
    }
}

/// The working-copy parent node — `sl whereami`, which answers in milliseconds where
/// `sl log` pays command dispatch (`specs/sapling.md` Reads). A merge in progress
/// prints two parents; the first line pins to the first parent.
pub fn parent_rev(root: &Path) -> Option<String> {
    let out = sl_out(root, &["whereami"]).ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next().unwrap_or("").trim();
    (!line.is_empty()).then(|| line.to_string())
}

// --- status ----------------------------------------------------------------------

/// One `sl status -Tjson` record: the JSON shape carries a copy source structurally,
/// where plain output rides it on an indented second line (`specs/sapling.md` Reads).
#[derive(Debug, Deserialize)]
struct StatusEntry {
    path: String,
    status: String,
    #[serde(default)]
    copy: Option<String>,
}

/// The dirty files vs the working-copy parent, or vs `rev` when given. Explicit
/// `-mardu` rather than the default show set, so a config cannot widen the pass into
/// the clean-file scan `sl help status` warns about.
fn status_entries(root: &Path, rev: Option<&str>) -> Result<Vec<StatusEntry>> {
    let mut args = vec!["status", "-mardu", "-C", "-Tjson"];
    if let Some(rev) = rev {
        args.extend(["--rev", rev]);
    }
    let out = sl(root, &args)?;
    serde_json::from_str(&out).context("parsing sl status -Tjson")
}

/// Map one status record to a changed-file kind (`specs/sapling.md` Reads).
fn kind_of(entry: &StatusEntry) -> Option<ChangeKind> {
    match entry.status.as_str() {
        "M" => Some(ChangeKind::Modified),
        "A" if entry.copy.is_some() => Some(ChangeKind::Renamed),
        "A" => Some(ChangeKind::Added),
        "R" | "!" => Some(ChangeKind::Deleted),
        "?" => Some(ChangeKind::Untracked),
        _ => None,
    }
}

// --- changed files ----------------------------------------------------------------

/// The changed files for `scope`, sorted by path (`specs/sapling.md` Scopes).
/// `last-turn` resolves through [`changed_against_snapshot`], so it lists nothing here.
pub fn changed_files(
    root: &Path,
    scope: Scope,
    branch_base: Option<&str>,
) -> Result<Vec<ChangedFile>> {
    match scope {
        Scope::Uncommitted => changed_set(root, None),
        Scope::Branch => match branch_base.and_then(|b| merge_base(root, b)) {
            Some(mb) => changed_set(root, Some(&mb)),
            None => Ok(Vec::new()),
        },
        Scope::LastTurn => Ok(Vec::new()),
    }
}

/// One scope build: the file list from `sl status`, the counts from one `sl diff`.
fn changed_set(root: &Path, rev: Option<&str>) -> Result<Vec<ChangedFile>> {
    let entries = status_entries(root, rev)?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let mut diff_args = vec!["diff", "--git"];
    if let Some(rev) = rev {
        diff_args.extend(["-r", rev]);
    }
    let counts = diff_counts(&sl(root, &diff_args)?);
    // A move reports twice: `A dest` carrying the copy source, plus `R source`. The
    // source row folds into the rename, matching git's single `R old new` record.
    let moved_sources: std::collections::HashSet<&str> =
        entries.iter().filter(|e| e.status == "A").filter_map(|e| e.copy.as_deref()).collect();
    let mut files: Vec<ChangedFile> = Vec::new();
    for entry in &entries {
        let Some(kind) = kind_of(entry) else { continue };
        if kind == ChangeKind::Deleted && moved_sources.contains(entry.path.as_str()) {
            continue;
        }
        let (additions, deletions) = match kind {
            // Untracked files never appear in `sl diff`; count locally like git's
            // untracked pass.
            ChangeKind::Untracked => (crate::vcs::line_additions(root, &entry.path), 0),
            // A plain-`rm` deletion (`!`) never reaches `sl diff` either; count its
            // old side at the scope's base, one cached read per deleted file.
            ChangeKind::Deleted if !counts.contains_key(&entry.path) => {
                let base = match rev {
                    Some(rev) => Some(rev.to_string()),
                    None => parent_rev(root),
                };
                let old = base.and_then(|rev| cat_cached(root, &rev, &entry.path).ok().flatten());
                (0, old.map_or(0, |b| crate::vcs::text_line_count(&b)))
            }
            _ => counts.get(&entry.path).copied().unwrap_or((0, 0)),
        };
        files.push(ChangedFile {
            path: entry.path.clone(),
            kind,
            additions,
            deletions,
            previous_path: entry.copy.clone(),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    Ok(files)
}

/// Per-file `(additions, deletions)` from `sl diff --git` output. Counts come from the
/// hunk lines, keyed under the new path from the `+++ b/` header (the `--- a/` header
/// for a deletion). A `---`/`+++` line inside a hunk is content — headers only occur
/// between a `diff --git` boundary and its first `@@`.
fn diff_counts(out: &str) -> HashMap<String, (u32, u32)> {
    let mut map = HashMap::new();
    let mut file: Option<String> = None;
    let mut old_name: Option<String> = None;
    let mut in_hunk = false;
    let mut add = 0u32;
    let mut del = 0u32;
    let flush =
        |file: &mut Option<String>, add: &mut u32, del: &mut u32, map: &mut HashMap<_, _>| {
            if let Some(path) = file.take() {
                map.insert(path, (*add, *del));
            }
            *add = 0;
            *del = 0;
        };
    for line in out.lines() {
        if line.starts_with("diff --git ") {
            flush(&mut file, &mut add, &mut del, &mut map);
            old_name = None;
            in_hunk = false;
            continue;
        }
        if !in_hunk {
            if let Some(old) = line.strip_prefix("--- a/") {
                old_name = Some(old.to_string());
                continue;
            }
            if let Some(new) = line.strip_prefix("+++ b/") {
                file = Some(new.to_string());
                continue;
            }
            if line.starts_with("+++ /dev/null") {
                file = old_name.take();
                continue;
            }
            if line.starts_with("@@") {
                in_hunk = true;
            }
            continue;
        }
        if line.starts_with("@@") {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => add += 1,
            Some(b'-') => del += 1,
            _ => {}
        }
    }
    flush(&mut file, &mut add, &mut del, &mut map);
    map
}

/// `(additions, deletions)` computed in-process from two contents — the `last-turn`
/// counts, whose baseline side exists only in the store (`specs/sapling.md`). Binary
/// content counts (0, 0), matching git's numstat.
fn text_counts(old: &[u8], new: &[u8]) -> (u32, u32) {
    if old.contains(&0) || new.contains(&0) {
        return (0, 0);
    }
    let old = String::from_utf8_lossy(old);
    let new = String::from_utf8_lossy(new);
    let diff = similar::TextDiff::from_lines(old.as_ref(), new.as_ref());
    let mut add = 0u32;
    let mut del = 0u32;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => add += 1,
            similar::ChangeTag::Delete => del += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (add, del)
}

// --- revision resolution ------------------------------------------------------------

/// Resolve one revision spelling to its full node, `Ok(None)` for a spelling Sapling
/// does not know — the chain's skip-never-error contract (`specs/review-model.md` Base
/// branch). `limit(<spelling>, 1)` bounds a typed revset to one answer, so a runaway
/// revset cannot stall the pane. A hex-shaped spelling resolves through `id()`, which
/// reads it as a hash prefix: bare in a revset, Sapling reads `123456` as a local
/// revision number and resolves a decade-old commit (`specs/sapling.md` Scopes).
fn resolve_rev(root: &Path, spelling: &str) -> Result<Option<String>, GitFail> {
    if spelling.is_empty() || spelling.starts_with('-') {
        return Ok(None);
    }
    let revset = probe_revset(spelling);
    let args = ["log", "-r", &revset, "-T", "{node}"];
    let out = sl_out(root, &args).map_err(|e| GitFail(format!("sl {args:?}: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // An unknown name, an unparsable revset, or an ambiguous prefix is a clean
        // miss — a pick whose prefix went ambiguous must go dormant, never fail every
        // build (`specs/review-model.md` Base branch). Anything else is a repository
        // failure the caller must not read as "no base".
        if stderr.contains("unknown revision")
            || stderr.contains("parse error")
            || stderr.contains("ambiguous identifier")
        {
            return Ok(None);
        }
        return Err(GitFail(format!("sl {args:?}: {}", stderr.trim())));
    }
    let node = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!node.is_empty()).then_some(node))
}

/// The bounded revset one spelling probes. Digit-only always routes through `id()`
/// whatever its length — bare, even `12` is a revnum. Mixed hex routes through `id()`
/// from git's own abbreviation floor, so a three-letter bookmark named `abc` still
/// resolves as a name.
fn probe_revset(spelling: &str) -> String {
    let all_digits = spelling.bytes().all(|b| b.is_ascii_digit());
    let hex_shaped =
        all_digits || (spelling.len() >= 4 && spelling.bytes().all(|b| b.is_ascii_hexdigit()));
    if hex_shaped { format!("limit(id({spelling}), 1)") } else { format!("limit({spelling}, 1)") }
}

/// The last public ancestor of the working copy — the default `branch` base. An empty
/// answer (a repo with no public commits) is a no-base state, never an error
/// (`specs/sapling.md` Scopes).
fn public_base(root: &Path) -> Result<Option<String>, GitFail> {
    resolve_rev(root, "last(public() & ::.)")
}

/// The merge base of `base_oid` and the working copy, through the immutable-ancestor
/// cache — `sl log` pays command dispatch, and `content_sides` runs on the frame loop.
/// The revset uses the pinned parent, never `.`: the working copy can move between the
/// pin and the spawn, and the cache key must name what was actually asked. A failed
/// command returns `None` uncached, so the next build retries — caching it would
/// freeze a transient failure into a permanently empty branch view
/// (`specs/overview.md` Continuity).
pub fn merge_base(root: &Path, base_oid: &str) -> Option<String> {
    let parent = parent_rev(root)?;
    let key = (base_oid.to_string(), parent);
    if let Some(hit) = ancestor_cache().lock().unwrap().get(&key) {
        return hit.clone();
    }
    let revset = format!("ancestor({base_oid}, {})", key.1);
    let node = match sl(root, &["log", "-r", &revset, "-T", "{node}"]) {
        Ok(out) => {
            let node = out.trim().to_string();
            (!node.is_empty()).then_some(node)
        }
        Err(e) => {
            logln!("sl merge_base failed: {e}");
            return None;
        }
    };
    let mut cache = ancestor_cache().lock().unwrap();
    if cache.len() >= 128 {
        cache.clear();
    }
    cache.insert(key, node.clone());
    node
}

/// A cache from a (revision, revision-or-path) pair to an immutable answer.
type PinnedCache<T> = Mutex<HashMap<(String, String), Option<T>>>;

/// The ancestor of two pinned commits never changes, so a hit can never be stale.
fn ancestor_cache() -> &'static PinnedCache<String> {
    static CACHE: OnceLock<PinnedCache<String>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// Resolve the base chain: the `--base` flag, then the worktree pick, then the public
/// base (`specs/sapling.md` Scopes). Every winner is a `Rev`: Sapling has no
/// origin-then-local branch walk, and a bookmark spelling paints with its pin.
pub fn resolve_base(root: &Path, base_flag: Option<&str>) -> Result<BaseStatus, GitFail> {
    let mut skipped: Option<String> = None;
    if let Some(flag) = base_flag.filter(|f| !f.is_empty()) {
        match resolve_rev(root, flag)? {
            Some(oid) => {
                return Ok(BaseStatus {
                    winner: Some(ResolvedBase::Rev { spelling: flag.to_string(), oid }),
                    skipped: None,
                });
            }
            None => skipped = Some(flag.to_string()),
        }
    }
    if let Some(pick) = Store::open(root).read_base_pick()? {
        match resolve_rev(root, &pick)? {
            Some(oid) => {
                return Ok(BaseStatus {
                    winner: Some(ResolvedBase::Rev { spelling: pick, oid }),
                    skipped,
                });
            }
            None => skipped = skipped.or(Some(pick)),
        }
    }
    // The spelling is the full node, so picking this row records an unambiguous pin —
    // a 7-hex prefix is routinely ambiguous in a monorepo. It still paints abbreviated,
    // since a hex prefix of its own oid paints once (`rev_paint`).
    let winner = public_base(root)?.map(|oid| ResolvedBase::Rev { spelling: oid.clone(), oid });
    Ok(BaseStatus { winner, skipped })
}

/// Resolve one typed base-picker spelling (`specs/input.md` Base picker).
pub fn resolve_spelling(root: &Path, spelling: &str) -> Result<Option<ResolvedBase>, GitFail> {
    Ok(resolve_rev(root, spelling)?
        .map(|oid| ResolvedBase::Rev { spelling: spelling.to_string(), oid }))
}

/// The local bookmarks, for the base picker (`specs/sapling.md` Scopes). A repo
/// without bookmarks lists nothing; the typed spelling is the usual path to a pick.
pub fn list_bookmarks(root: &Path) -> Result<Vec<String>, GitFail> {
    let args = ["bookmarks", "-T", "{bookmark}\n"];
    let out = sl_out(root, &args).map_err(|e| GitFail(format!("sl {args:?}: {e}")))?;
    if !out.status.success() {
        return Err(GitFail(format!(
            "sl {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// One stack commit offered by the base picker: the short node it records, and the
/// description's first line it reads as (`specs/sapling.md` Scopes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackCommit {
    pub node: String,
    pub title: String,
}

/// The draft ancestors of `.`, newest first, for the base picker (`specs/sapling.md`
/// Scopes). The working-copy parent is dropped: basing on it shows what `uncommitted`
/// shows. Scanning the stack rather than the repository's whole draft set keeps this
/// O(stack) (`SL-SCALE-CHANGED`).
pub fn list_stack(root: &Path) -> Result<Vec<StackCommit>, GitFail> {
    let args = ["log", "-r", "sort(draft() & ::., -rev)", "-T", "{node|short}\t{desc|firstline}\n"];
    let out = sl_out(root, &args).map_err(|e| GitFail(format!("sl {args:?}: {e}")))?;
    if !out.status.success() {
        return Err(GitFail(format!(
            "sl {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let parent = parent_rev(root);
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (node, title) = line.split_once('\t')?;
            let node = node.trim();
            if node.is_empty() {
                return None;
            }
            let parent_row =
                parent.as_deref().is_some_and(|p| p.starts_with(node) || node.starts_with(p));
            if parent_row {
                return None;
            }
            Some(StackCommit { node: node.to_string(), title: title.trim().to_string() })
        })
        .collect())
}

// --- file content -------------------------------------------------------------------

/// The content of `path` at `rev`, empty when absent there. `rev` is `.`, a pinned
/// node, or a `snap:` turn-baseline id (`specs/sapling.md`).
pub fn file_content(root: &Path, rev: &str, path: &str) -> String {
    if rev.starts_with(SNAP_PREFIX) {
        let store = Store::open(root);
        let Some(manifest) = store.load_manifest(rev) else { return String::new() };
        return match manifest.files.get(path) {
            Some(Some(hash)) => store
                .read_blob(hash)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default(),
            Some(None) => String::new(),
            None => cat_lossy(root, &manifest.parent, path),
        };
    }
    // Pin `.` before the cache: a cache keyed on the moving spelling would go stale
    // the moment the agent commits.
    let pinned;
    let rev = if rev == "." {
        match parent_rev(root) {
            Some(parent) => {
                pinned = parent;
                &pinned
            }
            None => return String::new(),
        }
    } else {
        rev
    };
    cat_lossy(root, rev, path)
}

fn cat_lossy(root: &Path, rev: &str, path: &str) -> String {
    cat_cached(root, rev, path)
        .ok()
        .flatten()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// `sl cat -r <rev> <path>` through the immutable-content cache: a commit's file
/// content never changes, so a hit can never be stale, and the frame loop's old-side
/// reads skip the command-dispatch floor after the first. Exit 1 is absence
/// (`specs/sapling.md` Reads).
fn cat_cached(root: &Path, rev: &str, path: &str) -> Result<Option<Vec<u8>>> {
    let key = (rev.to_string(), path.to_string());
    if let Some(hit) = cat_cache().lock().unwrap().get(&key) {
        return Ok(hit.clone());
    }
    let args = ["cat", "-r", rev, "--", path];
    let out = sl_out(root, &args).with_context(|| format!("running sl {args:?}"))?;
    let content = if out.status.success() {
        Some(out.stdout)
    } else if out.status.code() == Some(1) {
        None
    } else {
        bail!("sl {args:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    };
    let mut cache = cat_cache().lock().unwrap();
    // Bound both entries and bytes — cached monorepo files are not small. A wholesale
    // clear repopulates from immutable content, so it can never serve a wrong answer.
    let bytes: usize = cache.values().flatten().map(Vec::len).sum();
    if cache.len() >= 256 || bytes >= 64 * 1024 * 1024 {
        cache.clear();
    }
    cache.insert(key, content.clone());
    Ok(content)
}

fn cat_cache() -> &'static PinnedCache<Vec<u8>> {
    static CACHE: OnceLock<PinnedCache<Vec<u8>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

// --- the snapshot store ---------------------------------------------------------------
//
// See `specs/sapling.md` The snapshot store. One directory per worktree under
// reviewr's state dir. It holds the base pick, the turn-baseline pointer, the
// baseline manifests, and the content-addressed blobs of files dirty at snapshot
// time. Nothing here ever touches the repository (SL-NO-REPO-WRITES).

const SNAP_PREFIX: &str = "snap:";

/// A turn baseline: the working-copy parent plus the dirty files at snapshot time.
/// `None` content marks a path absent from the worktree (removed or missing). The
/// baseline id is a digest over this shape, so the divergence check compares ids
/// exactly as git compares tree ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    parent: String,
    files: BTreeMap<String, Option<String>>,
}

impl Manifest {
    fn id(&self) -> String {
        let canonical = serde_json::to_vec(self).expect("a manifest serializes");
        format!("{SNAP_PREFIX}{:x}", Sha256::digest(&canonical))
    }
}

/// The snapshot store handle for one worktree.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The store for `root`, keyed by the same worktree-path hash the git baseline ref
    /// uses. `HERDR_REVIEWR_STATE_DIR` overrides the state root for QA and tests.
    pub fn open(root: &Path) -> Self {
        let base = std::env::var_os("HERDR_REVIEWR_STATE_DIR")
            .map(PathBuf::from)
            .or_else(dirs::state_dir)
            .or_else(dirs::data_local_dir)
            .unwrap_or_else(std::env::temp_dir);
        Self { dir: base.join("herdr-reviewr").join("sl").join(crate::git::worktree_key(root)) }
    }

    /// A store at an explicit directory — the test seam.
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The store's directory, so a test that wrote through the default resolution can
    /// remove what it created.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn pick_path(&self) -> PathBuf {
        self.dir.join("base-pick")
    }

    fn baseline_path(&self) -> PathBuf {
        self.dir.join("baseline")
    }

    fn snaps_dir(&self) -> PathBuf {
        self.dir.join("snaps")
    }

    fn blobs_dir(&self) -> PathBuf {
        self.dir.join("blobs")
    }

    fn manifest_path(&self, id: &str) -> Option<PathBuf> {
        let hex = id.strip_prefix(SNAP_PREFIX)?;
        hex.bytes()
            .all(|b| b.is_ascii_hexdigit())
            .then(|| self.snaps_dir().join(format!("{hex}.json")))
    }

    /// Write `content` through a temp file and rename, so a crash never leaves a
    /// half-written pick or pointer.
    fn write_atomic(&self, path: &Path, content: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(self.dir.as_path())?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, path)
    }

    /// The recorded pick's spelling, or `None` when no pick is recorded. A missing file
    /// is no pick; a malformed spelling is no pick (`specs/review-model.md` Base branch).
    pub fn read_base_pick(&self) -> Result<Option<String>, GitFail> {
        let content = match std::fs::read_to_string(self.pick_path()) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(GitFail(format!("reading the base pick: {e}"))),
        };
        let name = content.trim();
        Ok(crate::git::pick_spelling_shaped(name).then(|| name.to_string()))
    }

    /// Record `name` as this worktree's pick (`specs/sapling.md` Scopes).
    pub fn write_base_pick(&self, name: &str) -> Result<(), GitFail> {
        self.write_atomic(&self.pick_path(), name.as_bytes())
            .map_err(|e| GitFail(format!("writing the base pick: {e}")))
    }

    /// Drop the recorded pick.
    pub fn clear_base_pick(&self) -> Result<(), GitFail> {
        match std::fs::remove_file(self.pick_path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(GitFail(format!("clearing the base pick: {e}"))),
        }
    }

    /// The persisted turn baseline id, if its manifest is still readable — a pointer
    /// whose manifest is gone must read as no baseline, or `last-turn` would fail
    /// every build until the next turn.
    pub fn read_baseline(&self) -> Option<String> {
        let id = std::fs::read_to_string(self.baseline_path()).ok()?;
        let id = id.trim().to_string();
        self.load_manifest(&id).map(|_| id)
    }

    fn load_manifest(&self, id: &str) -> Option<Manifest> {
        let bytes = std::fs::read(self.manifest_path(id)?).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn read_blob(&self, hash: &str) -> Option<Vec<u8>> {
        if !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        std::fs::read(self.blobs_dir().join(hash)).ok()
    }

    /// Persist `pending` and point the baseline at it: blobs, then the manifest, then
    /// the pointer — a crash mid-way leaves the previous baseline live. Every write is
    /// temp+rename: a sibling pane reads this store concurrently, and a torn manifest
    /// or blob must never parse as truncated content. A blob rename lands over an
    /// identical existing blob harmlessly, where an exists-then-skip check would race
    /// a sibling's prune sweep. Store entries no longer referenced drop afterwards
    /// (`specs/sapling.md` The snapshot store).
    fn persist(&self, pending: &Pending) -> Result<()> {
        std::fs::create_dir_all(self.blobs_dir()).context("creating the blob dir")?;
        std::fs::create_dir_all(self.snaps_dir()).context("creating the manifest dir")?;
        for (hash, bytes) in &pending.blobs {
            self.write_atomic(&self.blobs_dir().join(hash), bytes)
                .context("writing a baseline blob")?;
        }
        let manifest_path =
            self.manifest_path(&pending.id).context("a pending id is well-formed")?;
        self.write_atomic(&manifest_path, &serde_json::to_vec(&pending.manifest)?)
            .context("writing the baseline manifest")?;
        self.write_atomic(&self.baseline_path(), pending.id.as_bytes())
            .context("writing the baseline pointer")?;
        self.prune(&pending.id);
        Ok(())
    }

    /// Best-effort removal of manifests and blobs no recent baseline references. The
    /// just-written baseline plus the [`KEPT_MANIFESTS`] most recent others survive —
    /// a sibling pane of the same worktree keeps its own live baseline even after this
    /// pane promotes, at the cost of a bounded tail of old snapshots.
    fn prune(&self, keep_id: &str) {
        const KEPT_MANIFESTS: usize = 4;
        let keep_manifest = self.manifest_path(keep_id);
        let mut others: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(self.snaps_dir()) {
            for entry in entries.flatten() {
                if keep_manifest.as_deref() == Some(entry.path().as_path()) {
                    continue;
                }
                let modified = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                others.push((modified, entry.path()));
            }
        }
        others.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        for (_, path) in others.drain(..).skip(KEPT_MANIFESTS) {
            let _ = std::fs::remove_file(path);
        }
        // A blob survives while any kept manifest references it.
        let mut live: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Ok(entries) = std::fs::read_dir(self.snaps_dir()) {
            for entry in entries.flatten() {
                let Ok(bytes) = std::fs::read(entry.path()) else { continue };
                let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else { continue };
                live.extend(manifest.files.into_values().flatten());
            }
        }
        if let Ok(entries) = std::fs::read_dir(self.blobs_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !live.contains(&name) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// A snapshot built this session and not yet persisted: its manifest plus the dirty
/// contents, held in memory so a candidate's bytes are the turn-start bytes even
/// though persistence happens at promotion. Blobs are shared with the stat cache, so
/// a stat-unchanged file's bytes exist once however many snapshots hold them.
#[derive(Debug)]
struct Pending {
    id: String,
    manifest: Manifest,
    blobs: HashMap<String, std::sync::Arc<Vec<u8>>>,
}

/// One stat-gated dirty file: its bytes re-read only when the stat moves. The
/// divergence check snapshots every poll while a candidate pends, so an unchanged
/// 500MB artifact must not be re-read and re-hashed each time. A stat that lies
/// (same mtime and length after an edit) costs a missed divergence for one poll
/// window, the same wager git's index stat cache makes.
#[derive(Debug)]
struct StatEntry {
    mtime: std::time::SystemTime,
    len: u64,
    hash: String,
    bytes: std::sync::Arc<Vec<u8>>,
}

/// The worker-side snapshot host for one Sapling worktree: builds snapshots, holds the
/// pending candidate, and persists the baseline on promotion
/// (`specs/sapling.md` The snapshot store).
#[derive(Debug)]
pub struct TurnStore {
    store: Store,
    root: PathBuf,
    /// The most recent snapshot — the divergence check's "now" side, overwritten per poll.
    scratch: Option<Pending>,
    /// The pinned turn-start snapshot awaiting promotion.
    candidate: Option<Pending>,
    /// The per-path stat gate, kept to the current dirty set.
    stats: HashMap<String, StatEntry>,
}

impl TurnStore {
    pub fn open(root: PathBuf) -> Self {
        Self::with_store(Store::open(&root), root)
    }

    /// A store rooted at an explicit directory — the test seam.
    pub fn with_store(store: Store, root: PathBuf) -> Self {
        Self { store, root, scratch: None, candidate: None, stats: HashMap::new() }
    }

    pub fn read_baseline(&self) -> Option<String> {
        self.store.read_baseline()
    }

    /// Snapshot the worktree: the parent pin plus every dirty file's content, digested
    /// to an id. O(dirty files), never O(worktree) (`SL-SCALE-CHANGED`), and a
    /// stat-unchanged file skips its read and hash.
    pub fn snapshot(&mut self) -> Result<String> {
        let parent = parent_rev(&self.root).context("sl whereami gave no parent")?;
        let entries = status_entries(&self.root, None)?;
        let mut files: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut blobs: HashMap<String, std::sync::Arc<Vec<u8>>> = HashMap::new();
        for entry in entries {
            if kind_of(&entry).is_none() {
                continue;
            }
            // A file the status listed but the read misses just went away — record it
            // absent, exactly as an `R`/`!` entry.
            let hash = self.hashed_content(&entry.path).map(|(hash, bytes)| {
                blobs.insert(hash.clone(), bytes);
                hash
            });
            files.insert(entry.path, hash);
        }
        self.stats.retain(|path, _| files.contains_key(path));
        let manifest = Manifest { parent, files };
        let id = manifest.id();
        // The candidate's own digest re-observed is not a new snapshot; keeping the
        // scratch slot free preserves the pinned bytes.
        if self.candidate.as_ref().is_none_or(|c| c.id != id) {
            self.scratch = Some(Pending { id: id.clone(), manifest, blobs });
        }
        Ok(id)
    }

    /// One dirty file's content hash and bytes, through the stat gate; `None` when the
    /// file is absent.
    fn hashed_content(&mut self, path: &str) -> Option<(String, std::sync::Arc<Vec<u8>>)> {
        let meta = std::fs::metadata(self.root.join(path)).ok()?;
        let mtime = meta.modified().ok();
        if let (Some(mtime), Some(hit)) = (mtime, self.stats.get(path))
            && hit.mtime == mtime
            && hit.len == meta.len()
        {
            return Some((hit.hash.clone(), hit.bytes.clone()));
        }
        let bytes = std::sync::Arc::new(std::fs::read(self.root.join(path)).ok()?);
        let hash = format!("{:x}", Sha256::digest(bytes.as_slice()));
        if let Some(mtime) = mtime {
            self.stats.insert(
                path.to_string(),
                StatEntry {
                    mtime,
                    len: bytes.len() as u64,
                    hash: hash.clone(),
                    bytes: bytes.clone(),
                },
            );
        }
        Some((hash, bytes))
    }

    /// Pin the just-built snapshot as the turn-start candidate, so later polls'
    /// scratch snapshots cannot evict its bytes before promotion.
    pub fn pin_candidate(&mut self, id: &str) {
        if self.scratch.as_ref().is_some_and(|p| p.id == id) {
            self.candidate = self.scratch.take();
        }
    }

    /// Persist `id` as the live baseline. The bytes come from the pinned candidate
    /// (or the scratch slot when the two collapsed); an id from neither is a baseline
    /// this process never built, which only a bug produces. The slot is consumed only
    /// on success, so a failed persist (a full disk, an unwritable state dir) keeps
    /// the bytes for the next poll's retry.
    pub fn persist_baseline(&mut self, id: &str) -> Result<()> {
        let from_candidate = self.candidate.as_ref().is_some_and(|c| c.id == id);
        let pending = if from_candidate {
            self.candidate.as_ref()
        } else {
            self.scratch.as_ref().filter(|s| s.id == id)
        };
        let pending = pending.context("no pending snapshot matches the promoted baseline")?;
        self.store.persist(pending)?;
        if from_candidate {
            self.candidate = None;
        } else {
            self.scratch = None;
        }
        Ok(())
    }
}

/// The changed files between the persisted baseline `id` and the live worktree
/// (`specs/sapling.md` The snapshot store). Two candidate classes, each answered
/// without per-file subprocesses: a path in the baseline's dirty set compares the
/// stored bytes against the worktree in-process, and every other `sl status --rev
/// <parent>` path had the parent's content at the turn start, so its kind and counts
/// come from that status plus one `sl diff` — differing from the parent IS differing
/// from the baseline. Commits made during the turn stay in the diff either way.
pub fn changed_against_snapshot(root: &Path, id: &str) -> Result<Vec<ChangedFile>> {
    let store = Store::open(root);
    let manifest = store
        .load_manifest(id)
        .with_context(|| format!("turn baseline {id} is missing from the snapshot store"))?;
    let now = status_entries(root, Some(&manifest.parent))?;
    let mut out = Vec::new();
    // The turn-start-dirty files: the stored bytes are the baseline side.
    for (path, base_hash) in &manifest.files {
        let base: Option<Vec<u8>> = match base_hash {
            Some(hash) => Some(
                store
                    .read_blob(hash)
                    .with_context(|| format!("baseline blob for {path} is missing"))?,
            ),
            None => None,
        };
        let current: Option<Vec<u8>> = std::fs::read(root.join(path)).ok();
        let (kind, additions, deletions) = match (&base, &current) {
            (None, None) => continue,
            (Some(b), Some(c)) if b == c => continue,
            (None, Some(c)) => (ChangeKind::Added, crate::vcs::text_line_count(c), 0),
            (Some(b), None) => (ChangeKind::Deleted, 0, crate::vcs::text_line_count(b)),
            (Some(b), Some(c)) => {
                let (add, del) = text_counts(b, c);
                (ChangeKind::Modified, add, del)
            }
        };
        out.push(ChangedFile {
            path: path.clone(),
            kind,
            additions,
            deletions,
            previous_path: None,
        });
    }
    // The files the turn touched from a clean start: kinds from the status, counts
    // from one diff spawn. A rename's source row folds exactly as in `changed_set`.
    let fresh: Vec<&StatusEntry> =
        now.iter().filter(|e| !manifest.files.contains_key(&e.path)).collect();
    if !fresh.is_empty() {
        let counts = diff_counts(&sl(root, &["diff", "--git", "-r", &manifest.parent])?);
        let moved_sources: std::collections::HashSet<&str> =
            fresh.iter().filter(|e| e.status == "A").filter_map(|e| e.copy.as_deref()).collect();
        for entry in fresh {
            let Some(kind) = kind_of(entry) else { continue };
            if kind == ChangeKind::Deleted && moved_sources.contains(entry.path.as_str()) {
                continue;
            }
            // The baseline has no untracked concept: a file created during the turn is
            // an addition, matching git's tree diff.
            let kind = if kind == ChangeKind::Untracked { ChangeKind::Added } else { kind };
            let (additions, deletions) = match kind {
                ChangeKind::Added if entry.status == "?" => {
                    (crate::vcs::line_additions(root, &entry.path), 0)
                }
                // A plain-`rm` deletion (`!`) never reaches `sl diff`; count its old
                // side, one cached read per deleted file.
                ChangeKind::Deleted if !counts.contains_key(&entry.path) => {
                    let old = cat_cached(root, &manifest.parent, &entry.path).ok().flatten();
                    (0, old.map_or(0, |b| crate::vcs::text_line_count(&b)))
                }
                _ => counts.get(&entry.path).copied().unwrap_or((0, 0)),
            };
            out.push(ChangedFile {
                path: entry.path.clone(),
                kind,
                additions,
                deletions,
                previous_path: entry.copy.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{Manifest, Pending, Store, diff_counts, probe_revset, text_counts};

    #[test]
    fn probe_revset_reads_hex_as_a_prefix_and_names_as_names() {
        // Digit-only is a hash prefix at any length: bare, `123456` is a revnum and
        // would pin an ancient commit (`specs/sapling.md` Scopes).
        assert_eq!(probe_revset("123456"), "limit(id(123456), 1)");
        assert_eq!(probe_revset("0"), "limit(id(0), 1)");
        assert_eq!(probe_revset("beef1234"), "limit(id(beef1234), 1)");
        // Short mixed hex stays a name, so a bookmark named `abc` resolves.
        assert_eq!(probe_revset("abc"), "limit(abc, 1)");
        assert_eq!(probe_revset("master"), "limit(master, 1)");
        assert_eq!(probe_revset("last(public() & ::.)"), "limit(last(public() & ::.), 1)");
    }
    use std::collections::{BTreeMap, HashMap};

    #[test]
    fn diff_counts_keys_hunk_lines_under_the_new_path() {
        let out = "diff --git a/src/a.rs b/src/a.rs\n\
                   --- a/src/a.rs\n\
                   +++ b/src/a.rs\n\
                   @@ -1,2 +1,3 @@\n \
                   ctx\n\
                   +new line\n\
                   -old line\n\
                   +--- content that looks like a header\n\
                   diff --git a/gone.rs b/gone.rs\n\
                   --- a/gone.rs\n\
                   +++ /dev/null\n\
                   @@ -1,2 +0,0 @@\n\
                   -one\n\
                   -two\n";
        let m = diff_counts(out);
        assert_eq!(m["src/a.rs"], (2, 1));
        assert_eq!(m["gone.rs"], (0, 2));
    }

    #[test]
    fn diff_counts_reads_a_rename_under_its_new_name() {
        let out = "diff --git a/old.rs b/new.rs\n\
                   rename from old.rs\n\
                   rename to new.rs\n\
                   --- a/old.rs\n\
                   +++ b/new.rs\n\
                   @@ -1 +1 @@\n\
                   -a\n\
                   +b\n";
        let m = diff_counts(out);
        assert_eq!(m["new.rs"], (1, 1));
        assert!(!m.contains_key("old.rs"));
    }

    #[test]
    fn text_counts_diff_lines_and_binary_is_zero() {
        assert_eq!(text_counts(b"a\nb\n", b"a\nc\nd\n"), (2, 1));
        assert_eq!(text_counts(b"", b"x\n"), (1, 0));
        assert_eq!(text_counts(b"bin\0", b"x"), (0, 0));
    }

    #[test]
    fn manifest_id_is_stable_and_content_sensitive() {
        let manifest = |parent: &str, path: &str| Manifest {
            parent: parent.into(),
            files: BTreeMap::from([(path.to_string(), Some("abc".to_string()))]),
        };
        assert_eq!(manifest("p", "a.rs").id(), manifest("p", "a.rs").id());
        assert_ne!(manifest("p", "a.rs").id(), manifest("p", "b.rs").id());
        assert_ne!(manifest("p", "a.rs").id(), manifest("q", "a.rs").id());
        assert!(manifest("p", "a.rs").id().starts_with("snap:"));
    }

    #[test]
    fn store_roundtrips_pick_baseline_and_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_path_buf());
        // Pick: absent, written, cleared. A malformed spelling is no pick.
        assert_eq!(store.read_base_pick().unwrap(), None);
        store.write_base_pick("remote/main").unwrap();
        assert_eq!(store.read_base_pick().unwrap(), Some("remote/main".to_string()));
        store.clear_base_pick().unwrap();
        assert_eq!(store.read_base_pick().unwrap(), None);
        store.write_base_pick("-bad").unwrap();
        assert_eq!(store.read_base_pick().unwrap(), None);
        // Baseline: a pointer without a manifest reads as no baseline; a persisted
        // pending roundtrips manifest and blob.
        assert_eq!(store.read_baseline(), None);
        let manifest = Manifest {
            parent: "p".into(),
            files: BTreeMap::from([
                ("kept.rs".to_string(), Some("aa11".to_string())),
                ("gone.rs".to_string(), None),
            ]),
        };
        let id = manifest.id();
        let pending = Pending {
            id: id.clone(),
            manifest: manifest.clone(),
            blobs: HashMap::from([("aa11".to_string(), std::sync::Arc::new(b"hello".to_vec()))]),
        };
        store.persist(&pending).unwrap();
        assert_eq!(store.read_baseline(), Some(id.clone()));
        assert_eq!(store.load_manifest(&id), Some(manifest));
        assert_eq!(store.read_blob("aa11"), Some(b"hello".to_vec()));
    }

    #[test]
    fn persist_keeps_a_recent_tail_and_prunes_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_path_buf());
        let mut ids = Vec::new();
        for i in 0..6 {
            let hash = format!("aa{i:02}");
            let manifest = Manifest {
                parent: format!("p{i}"),
                files: BTreeMap::from([(format!("f{i}.rs"), Some(hash.clone()))]),
            };
            let id = manifest.id();
            let blobs = HashMap::from([(hash, std::sync::Arc::new(format!("v{i}").into_bytes()))]);
            store.persist(&Pending { id: id.clone(), manifest, blobs }).unwrap();
            ids.push(id);
        }
        assert_eq!(store.read_baseline(), Some(ids[5].clone()));
        // The newest and a recent tail survive, so a sibling pane's live baseline is
        // never yanked; only the oldest fall off.
        assert!(store.load_manifest(&ids[5]).is_some());
        assert!(store.load_manifest(&ids[1]).is_some(), "the kept tail survives");
        assert!(store.load_manifest(&ids[0]).is_none(), "the oldest is pruned");
        assert_eq!(store.read_blob("aa00"), None, "a pruned manifest's blob goes with it");
        assert_eq!(store.read_blob("aa05"), Some(b"v5".to_vec()));
    }
}
