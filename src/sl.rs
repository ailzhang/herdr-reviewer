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
use crate::vcs::{BranchEnds, Tip};

/// Run `sl <args>` at `root` and return its raw output.
fn sl_out(root: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    crate::proc::command("sl").current_dir(root).env("HGPLAIN", "1").args(args).output()
}

/// A failed command names itself the way it would be typed, `sl status -mardu -C -Tjson`,
/// because this text is what the status line shows the reviewer (`specs/sapling.md`
/// Failure semantics). An argument carrying whitespace or a control character prints
/// quoted and escaped, so a template's literal newline cannot break the line in two.
fn cmdline(args: &[&str]) -> String {
    let mut out = String::from("sl");
    for arg in args {
        out.push(' ');
        if arg.is_empty() || arg.chars().any(|c| c.is_whitespace() || c.is_control()) {
            out.push('\'');
            out.extend(arg.escape_debug());
            out.push('\'');
        } else {
            out.push_str(arg);
        }
    }
    out
}

/// Run `sl <args>` and return stdout. Errors on non-zero exit.
fn sl(root: &Path, args: &[&str]) -> Result<String> {
    let out = sl_out(root, args).with_context(|| format!("running {}", cmdline(args)))?;
    if !out.status.success() {
        bail!("{} failed: {}", cmdline(args), String::from_utf8_lossy(&out.stderr).trim());
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

/// The files that differ between `from` and `to`, each defaulting to the working-copy
/// parent and the working copy. Explicit show flags rather than the default set, so a
/// config cannot widen the pass into the clean-file scan `sl help status` warns about. A
/// commit-to-commit range drops the untracked flag: it has no working copy to scan.
fn status_entries(root: &Path, from: Option<&str>, to: Option<&str>) -> Result<Vec<StatusEntry>> {
    let show = if to.is_some() { "-mard" } else { "-mardu" };
    let mut args = vec!["status", show, "-C", "-Tjson"];
    for rev in [from, to].into_iter().flatten() {
        args.extend(["--rev", rev]);
    }
    let out = sl(root, &args)?;
    serde_json::from_str(&out).context("parsing sl status -Tjson")
}

/// Map one status record to a changed-file kind (`specs/sapling.md` Reads). `renamed` is
/// [`rename_sources`]'s verdict on this record's copy source.
fn kind_of(entry: &StatusEntry, renamed: bool) -> Option<ChangeKind> {
    match entry.status.as_str() {
        "M" => Some(ChangeKind::Modified),
        "A" if renamed => Some(ChangeKind::Renamed),
        "A" => Some(ChangeKind::Added),
        "R" | "!" => Some(ChangeKind::Deleted),
        "?" => Some(ChangeKind::Untracked),
        _ => None,
    }
}

/// The copy sources a rename consumed. `sl mv` reports `A dest` carrying the source plus
/// `R source`; `sl cp` reports the `A dest` alone and leaves the source in place, so a copy
/// reviews as an added file (`specs/sapling.md` Reads).
fn rename_sources<'a>(entries: &[&'a StatusEntry]) -> std::collections::HashSet<&'a str> {
    let removed: std::collections::HashSet<&str> =
        entries.iter().filter(|e| e.status == "R").map(|e| e.path.as_str()).collect();
    entries
        .iter()
        .filter(|e| e.status == "A")
        .filter_map(|e| e.copy.as_deref())
        .filter(|source| removed.contains(source))
        .collect()
}

/// The new side's line count, for a file whose whole content reads as an addition. The far
/// end of a pinned range has no worktree file to read, so it reads at that revision.
fn new_side_lines(root: &Path, to: Option<&str>, path: &str) -> u32 {
    match to {
        Some(rev) => cat_cached(root, rev, path)
            .ok()
            .flatten()
            .map_or(0, |b| crate::vcs::text_line_count(&b)),
        None => crate::vcs::line_additions(root, path),
    }
}

// --- changed files ----------------------------------------------------------------

/// The changed files for `scope`, sorted by path (`specs/sapling.md` Scopes).
/// `last-turn` resolves through [`changed_against_snapshot`], so it lists nothing here.
pub fn changed_files(
    root: &Path,
    scope: Scope,
    branch_base: Option<&str>,
    branch_tip: Option<&str>,
) -> Result<Vec<ChangedFile>> {
    match scope {
        Scope::Uncommitted => changed_set(root, None, None),
        Scope::Branch => match (branch_base, branch_tip) {
            // A pinned far end diffs commit to commit. The base is that commit's own
            // parent, so no merge base stands between the two (`specs/sapling.md` Scopes).
            (Some(base), Some(tip)) => changed_set(root, Some(base), Some(tip)),
            (Some(base), None) => match merge_base(root, base) {
                Some(mb) => changed_set(root, Some(&mb), None),
                None => Ok(Vec::new()),
            },
            (None, _) => Ok(Vec::new()),
        },
        Scope::LastTurn => Ok(Vec::new()),
    }
}

/// One scope build: the file list from `sl status`, the counts from one `sl diff`. A range
/// pinned at both ends is served from [`range_cache`], so reviewing one commit costs its
/// own resolution and nothing more per poll.
fn changed_set(root: &Path, from: Option<&str>, to: Option<&str>) -> Result<Vec<ChangedFile>> {
    let key = from.zip(to).map(|(f, t)| (f.to_string(), t.to_string()));
    if let Some(key) = &key
        && let Some(hit) = range_cache().lock().unwrap().get(key)
    {
        return Ok(hit.clone());
    }
    let files = build_changed_set(root, from, to)?;
    if let Some(key) = key {
        let mut cache = range_cache().lock().unwrap();
        // A wholesale clear repopulates from immutable content, so it can never serve a
        // wrong answer.
        if cache.len() >= 32 {
            cache.clear();
        }
        cache.insert(key, files.clone());
    }
    Ok(files)
}

/// A cache from a commit-to-commit range to the files it changed.
type RangeCache = Mutex<HashMap<(String, String), Vec<ChangedFile>>>;

/// Two pinned commits never change, so the changed set between them never changes. A
/// range that ends at the working copy is absent from this cache: the working copy moves
/// under it (`specs/sapling.md` Reads).
fn range_cache() -> &'static RangeCache {
    static CACHE: OnceLock<RangeCache> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn build_changed_set(
    root: &Path,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<ChangedFile>> {
    let entries = status_entries(root, from, to)?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let mut diff_args = vec!["diff", "--git", "--no-binary"];
    for rev in [from, to].into_iter().flatten() {
        diff_args.extend(["-r", rev]);
    }
    let counts = diff_counts(&sl(root, &diff_args)?);
    // A move reports twice: `A dest` carrying the copy source, plus `R source`. The
    // source row folds into the rename, matching git's single `R old new` record.
    let refs: Vec<&StatusEntry> = entries.iter().collect();
    let renamed = rename_sources(&refs);
    let mut files: Vec<ChangedFile> = Vec::new();
    for entry in &entries {
        let source = entry.copy.as_deref().filter(|s| renamed.contains(s));
        let Some(kind) = kind_of(entry, source.is_some()) else { continue };
        if kind == ChangeKind::Deleted && renamed.contains(entry.path.as_str()) {
            continue;
        }
        let (additions, deletions) = match kind {
            // Neither side reaches `sl diff` as an addition against nothing: an untracked
            // file is absent from it, and a copy diffs against the file it came from. Both
            // review as whole new files, so both count their new side.
            ChangeKind::Untracked => (new_side_lines(root, to, &entry.path), 0),
            ChangeKind::Added if entry.copy.is_some() => (new_side_lines(root, to, &entry.path), 0),
            // A plain-`rm` deletion (`!`) never reaches `sl diff` either; count its
            // old side at the scope's base, one cached read per deleted file.
            ChangeKind::Deleted if !counts.contains_key(&entry.path) => {
                let base = match from {
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
            previous_path: source.map(str::to_string),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    Ok(files)
}

/// The path a `---`/`+++` header names: everything up to the first tab. `sl diff --git`
/// appends a tab when the path holds a space, the unified-diff convention for separating
/// the name from a trailing field, and the untrimmed name matches no status path.
fn header_path(rest: &str) -> String {
    rest.split('\t').next().unwrap_or(rest).to_string()
}

/// Per-file `(additions, deletions)` from `sl diff --git` output. Counts come from the
/// hunk lines, keyed under the new path from the `+++ b/` header (the `--- a/` header
/// for a deletion). A `---`/`+++` line inside a hunk is content — headers only occur
/// between a `diff --git` boundary and its first `@@`.
///
/// A binary file has no hunk, so it lands in no key and reads as `(0, 0)`, the same
/// answer git's numstat gives for its `-`/`-` row (`specs/sapling.md` Reads).
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
                old_name = Some(header_path(old));
                continue;
            }
            if let Some(new) = line.strip_prefix("+++ b/") {
                file = Some(header_path(new));
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
///
/// A pair over the differ's budget counts (0, 0) too. Every other scope's counts come from
/// `sl diff`, which streams; these run Myers on the world worker every poll, and a pair that
/// is both large and heavily changed never finishes. The file the pane refuses to render is
/// exactly the file whose counts stay unread.
fn text_counts(old: &[u8], new: &[u8]) -> (u32, u32) {
    if old.contains(&0) || new.contains(&0) {
        return (0, 0);
    }
    let old = String::from_utf8_lossy(old);
    let new = String::from_utf8_lossy(new);
    if crate::diff::over_diff_budget(&old, &new) {
        return (0, 0);
    }
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

/// A recorded base pick: where the `branch` range starts, and the commit it ends at when
/// the pick names one commit to review. The picker records a commit row as
/// `<node>^..<node>` (`specs/sapling.md` Scopes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub base: String,
    pub tip: Option<String>,
}

impl Pick {
    /// Read one recorded line. A line with no `..` is a plain base, which is every pick a
    /// bookmark row, a typed spelling, or an older reviewr wrote.
    fn parse(line: &str) -> Self {
        match line.split_once("..") {
            Some((base, tip)) if !base.is_empty() && !tip.is_empty() => {
                Self { base: base.to_string(), tip: Some(tip.to_string()) }
            }
            _ => Self { base: line.to_string(), tip: None },
        }
    }

    /// What the header names when this pick goes dormant: the commit it reviewed, since
    /// that is the end the reviewer chose (`specs/review-model.md` Base branch). A node
    /// names itself abbreviated — a Sapling pick records the node whole, which would fill
    /// the header (`specs/sapling.md` Scopes).
    fn label(&self) -> String {
        let end = self.tip.clone().unwrap_or_else(|| self.base.clone());
        let node = end.len() > 12 && end.bytes().all(|b| b.is_ascii_hexdigit());
        if node { abbreviate_node(&end) } else { end }
    }
}

/// The abbreviated node every Sapling surface paints (`specs/sapling.md` Scopes).
/// Sapling's own short-node width, so a node the pane shows reads the same as the one
/// `sl` shows and pastes back into `sl` unchanged. Git's seven is two things here: below
/// the seven-hex floor a lazy monorepo clone will resolve a prefix at, and ambiguous at
/// monorepo scale, so the header would name commits the reviewer cannot look up.
#[must_use]
pub fn abbreviate_node(oid: &str) -> String {
    const N: usize = 12;
    if oid.len() <= N { oid.to_string() } else { oid[..N].to_string() }
}

/// Resolve one revision spelling to its full node, `Ok(None)` for a spelling Sapling
/// does not know — the chain's skip-never-error contract (`specs/review-model.md` Base
/// branch). `limit(<spelling>, 1)` bounds a typed revset to one answer, so a runaway
/// revset cannot stall the pane. A hex-shaped spelling resolves through `id()`, which
/// reads it as a hash prefix: bare in a revset, Sapling reads `123456` as a local
/// revision number and resolves a decade-old commit (`specs/sapling.md` Scopes).
fn resolve_rev(root: &Path, spelling: &str) -> Result<Option<String>, GitFail> {
    if unusable(spelling) {
        return Ok(None);
    }
    log_node(root, &probe_revset(spelling))
}

/// A spelling reviewr never hands to `sl log -r`: empty, or flag-shaped, which the
/// command would read as one of its own options.
fn unusable(spelling: &str) -> bool {
    spelling.is_empty() || spelling.starts_with('-')
}

/// Run one revset and read the node it names, `Ok(None)` when it names none.
fn log_node(root: &Path, revset: &str) -> Result<Option<String>, GitFail> {
    log_line(root, revset, "{node}")
}

/// Run one revset and read the single line `template` prints for it, `Ok(None)` when the
/// revset matches nothing. One spawn costs a third of a second in a monorepo, so a caller
/// that needs two facts about one commit asks for both in one template.
fn log_line(root: &Path, revset: &str, template: &str) -> Result<Option<String>, GitFail> {
    let args = ["log", "-r", revset, "-T", template];
    let out = sl_out(root, &args).map_err(|e| GitFail(format!("{}: {e}", cmdline(&args))))?;
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
        return Err(GitFail(format!("{}: {}", cmdline(&args), stderr.trim())));
    }
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!line.is_empty()).then_some(line))
}

/// The bounded revset one spelling probes. Only digit-only routes through `id()`, which
/// reads it as a hash prefix: bare, even `12` is a local revision number and pins whatever
/// commit that numbered a decade ago. Every other spelling goes bare, so one probe answers
/// a bookmark and a hash prefix alike, and a bookmark named `beef` is not unreachable for
/// being spelled in hex (`specs/sapling.md` Scopes).
///
/// `present()` holds the name inside the revset. A bare unknown symbol that looks like a
/// remote one is pulled from the remote before Sapling gives up on it, which is a
/// repository write (`SL-NO-REPO-WRITES`) on a path that runs per keystroke and per poll.
/// It also turns the miss into empty output rather than an abort, which is what the
/// chain's skip-never-error contract wants.
fn probe_revset(spelling: &str) -> String {
    if revnum_shaped(spelling) {
        format!("limit(present(id({spelling})), 1)")
    } else {
        format!("limit(present({spelling}), 1)")
    }
}

/// Whether [`probe_revset`] must fence `spelling` inside `id()` to keep Sapling from
/// reading it as a local revision number.
fn revnum_shaped(spelling: &str) -> bool {
    !spelling.is_empty() && spelling.bytes().all(|b| b.is_ascii_digit())
}

/// Whether [`complete_pick_spelling`] reads `spelling` as a hash prefix. Git's
/// abbreviation floor, so a three-letter bookmark named `abc` records as itself.
fn hex_shaped(spelling: &str) -> bool {
    revnum_shaped(spelling)
        || (spelling.len() >= 4 && spelling.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The spelling a typed hash prefix records: the whole node it resolved to
/// (`specs/sapling.md` Scopes). A monorepo holds enough commits that a typed prefix goes
/// ambiguous later, and an ambiguous pick is skipped, so the reviewer's pinned base would
/// quietly become the public base. A name records as itself and keeps following its
/// bookmark. The header paints the node abbreviated either way (`rev_paint`).
///
/// A bookmark resolves to a commit its own name does not prefix, so the `starts_with` test
/// tells the two apart on its own. Only a bookmark named for a hex prefix of the very
/// commit it points at records the node, and it records the commit the reviewer just
/// picked.
#[must_use]
pub fn complete_pick_spelling(spelling: &str, oid: &str) -> String {
    if hex_shaped(spelling) && oid.starts_with(spelling) {
        oid.to_string()
    } else {
        spelling.to_string()
    }
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
/// origin-then-local branch walk, and a bookmark spelling paints with its pin. Only the
/// pick can pin the range's far end — the flag names a base, exactly as git's does.
pub fn resolve_base(root: &Path, base_flag: Option<&str>) -> Result<BranchEnds, GitFail> {
    let ends = |winner, skipped, tip| BranchEnds { base: BaseStatus { winner, skipped }, tip };
    let mut skipped: Option<String> = None;
    if let Some(flag) = base_flag.filter(|f| !f.is_empty()) {
        match resolve_rev(root, flag)? {
            Some(oid) => {
                let winner = ResolvedBase::Rev { spelling: flag.to_string(), oid };
                return Ok(ends(Some(winner), None, None));
            }
            None => skipped = Some(flag.to_string()),
        }
    }
    if let Some(pick) = Store::open(root).read_base_pick()? {
        match resolve_pick(root, &pick)? {
            Some((winner, tip)) => return Ok(ends(Some(winner), skipped, tip)),
            None => skipped = skipped.or_else(|| Some(pick.label())),
        }
    }
    // The spelling is the full node, so picking this row records an unambiguous pin —
    // a 7-hex prefix is routinely ambiguous in a monorepo. It still paints abbreviated,
    // since a hex prefix of its own oid paints once (`rev_paint`).
    let winner = public_base(root)?.map(|oid| ResolvedBase::Rev { spelling: oid.clone(), oid });
    Ok(ends(winner, skipped, None))
}

/// Resolve one recorded pick to the range's ends. A commit pick needs both of them, so a
/// commit that has gone away skips the whole pick (`specs/sapling.md` Scopes).
fn resolve_pick(root: &Path, pick: &Pick) -> Result<Option<(ResolvedBase, Option<Tip>)>, GitFail> {
    let Some(tip) = &pick.tip else {
        let Some(base_oid) = resolve_rev(root, &pick.base)? else { return Ok(None) };
        let winner = ResolvedBase::Rev { spelling: pick.base.clone(), oid: base_oid };
        return Ok(Some((winner, None)));
    };
    if unusable(tip) {
        return Ok(None);
    }
    // The recorded base is the tip's parent, so every end and its title come from one
    // commit and one spawn. Reading the recorded spelling instead would pair an amended
    // commit with the parent of the node it replaced, the same commit only until a rebase.
    // The review number rides the same template. It holds no space, so it splits off ahead
    // of the description exactly as the two nodes do, and an unsubmitted commit's empty
    // field leaves the description in the last position all the same.
    let template = "{node} {p1node} {phabdiff} {desc|firstline}";
    let Some(line) = log_line(root, &successor_revset(tip), template)? else { return Ok(None) };
    let mut fields = line.splitn(4, ' ');
    let (Some(tip_oid), Some(base_oid)) = (fields.next(), fields.next()) else { return Ok(None) };
    let diff = fields.next().unwrap_or("").trim().to_string();
    // A root commit's parent is the null node. The range needs both ends, so the pick is
    // skipped exactly as one whose commit has gone away.
    if base_oid.bytes().all(|b| b == b'0') {
        return Ok(None);
    }
    let far = Tip {
        oid: tip_oid.to_string(),
        diff,
        title: fields.next().unwrap_or("").trim().to_string(),
    };
    // The base paints as its own node, never as the `<node>^` that spelled it: the header
    // already names the far end, and `spelling (oid)` would print the pair twice.
    let winner = ResolvedBase::Rev { spelling: base_oid.to_string(), oid: base_oid.to_string() };
    Ok(Some((winner, Some(far))))
}

/// The revset a picked commit resolves through: itself while it is live, and the node that
/// replaced it once `amend` or `rebase` has obsoleted it (`specs/sapling.md` Scopes).
/// `successors` is transitive and holds the commit itself, so a live commit answers itself
/// and a chain of amends answers its newest node. `last(sort(.., rev))` picks the newest of
/// a divergent set, and every `limit` bounds the walk to one answer.
fn successor_revset(spelling: &str) -> String {
    format!("limit(last(sort(successors({}) - obsolete(), rev)), 1)", probe_revset(spelling))
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
    let out = sl_out(root, &args).map_err(|e| GitFail(format!("{}: {e}", cmdline(&args))))?;
    if !out.status.success() {
        return Err(GitFail(format!(
            "{}: {}",
            cmdline(&args),
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

/// One stack commit offered by the base picker: the short node it records, the code
/// review number it carries, and the description's first line it reads as
/// (`specs/sapling.md` Scopes). `diff` is empty on a commit that has none, and on every
/// commit in a repository whose Sapling has no code review integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackCommit {
    pub node: String,
    pub diff: String,
    pub title: String,
}

/// The draft commits connected to `.`, newest first, for the base picker
/// (`specs/sapling.md` Scopes). Both directions: `sl prev` down the stack to amend a
/// commit is the review loop itself, and an ancestors-only walk would drop every commit
/// the reviewer just came from. Draft-only keeps this O(stack) (`SL-SCALE-CHANGED`).
///
/// Each commit resolves through its successors, so a working copy left on an obsolete
/// commit lists the node that replaced it, never the node it sits on. A successor that
/// landed is public, so `& draft()` drops the row rather than offering a commit that is no
/// longer in anyone's stack.
pub fn list_stack(root: &Path) -> Result<Vec<StackCommit>, GitFail> {
    let revset = "sort((successors(draft() & ((::.) + (.::))) & draft()) - obsolete(), -rev)";
    // The review number rides the same template, so the picker still costs one spawn.
    let args = ["log", "-r", revset, "-T", "{node|short}\t{phabdiff}\t{desc|firstline}\n"];
    let out = sl_out(root, &args).map_err(|e| GitFail(format!("{}: {e}", cmdline(&args))))?;
    if !out.status.success() {
        return Err(GitFail(format!(
            "{}: {}",
            cmdline(&args),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let node = fields.next()?.trim();
            let diff = fields.next()?.trim();
            let title = fields.next()?.trim();
            if node.is_empty() {
                return None;
            }
            Some(StackCommit {
                node: node.to_string(),
                diff: diff.to_string(),
                title: title.to_string(),
            })
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
    let out = sl_out(root, &args).with_context(|| format!("running {}", cmdline(&args)))?;
    let content = if out.status.success() {
        Some(out.stdout)
    } else if out.status.code() == Some(1) {
        None
    } else {
        bail!("{} failed: {}", cmdline(&args), String::from_utf8_lossy(&out.stderr).trim());
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

    /// The recorded pick, or `None` when no pick is recorded. A missing file is no pick;
    /// a malformed spelling is no pick (`specs/review-model.md` Base branch).
    pub fn read_base_pick(&self) -> Result<Option<Pick>, GitFail> {
        let content = match std::fs::read_to_string(self.pick_path()) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(GitFail(format!("reading the base pick: {e}"))),
        };
        let name = content.trim();
        Ok(crate::git::pick_spelling_shaped(name).then(|| Pick::parse(name)))
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

    /// The persisted turn baseline id, if the store still holds the whole snapshot — a
    /// pointer whose manifest is gone must read as no baseline, or `last-turn` would fail
    /// every build until the next turn. A blob is as load-bearing as the manifest naming
    /// it, and a store swept by an older reviewr can be missing one, so the seed checks
    /// every blob too. That is one `stat` per turn-start-dirty file, at open only.
    pub fn read_baseline(&self) -> Option<String> {
        let id = std::fs::read_to_string(self.baseline_path()).ok()?;
        let id = id.trim().to_string();
        let manifest = self.load_manifest(&id)?;
        let whole = manifest.files.values().flatten().all(|h| self.blobs_dir().join(h).is_file());
        whole.then_some(id)
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
    ///
    /// Every step keeps on doubt. A sweep that cannot prove a blob dead leaves it for the
    /// next promotion, because the loss it would otherwise take is a live baseline's
    /// content, and the loss it takes instead is one round of disk.
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
        // A blob survives while any kept manifest references it. The sweep fails closed:
        // one unreadable manifest, or a listing that fails, leaves live blobs unnamed, and
        // deleting on that reading would drop the baseline the failed read was holding.
        let mut live: std::collections::HashSet<String> = std::collections::HashSet::new();
        let Ok(entries) = std::fs::read_dir(self.snaps_dir()) else { return };
        for entry in entries {
            let Ok(entry) = entry else { return };
            let Ok(bytes) = std::fs::read(entry.path()) else { return };
            let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else { return };
            live.extend(manifest.files.into_values().flatten());
        }
        let Ok(entries) = std::fs::read_dir(self.blobs_dir()) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if live.contains(&name) || written_recently(&entry) {
                continue;
            }
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Whether a blob is young enough that a sibling pane may be mid-promotion with it.
/// Blobs land before the manifest naming them, so a blob written between a sweep's
/// manifest scan and its blob scan is referenced by nothing on disk yet. The grace window
/// keeps it until the next sweep, which is one promotion later (`specs/sapling.md` The
/// snapshot store). An unreadable stat keeps the blob.
fn written_recently(entry: &std::fs::DirEntry) -> bool {
    const WRITE_GRACE: std::time::Duration = std::time::Duration::from_mins(1);
    let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { return true };
    match std::time::SystemTime::now().duration_since(modified) {
        Ok(age) => age < WRITE_GRACE,
        // An mtime ahead of the clock is a skewed write, never an old blob.
        Err(_) => true,
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
        let entries = status_entries(&self.root, None, None)?;
        let mut files: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut blobs: HashMap<String, std::sync::Arc<Vec<u8>>> = HashMap::new();
        for entry in entries {
            // A rename and a copy both snapshot as their own path, so the kind is read
            // only to drop a status the review does not track.
            if kind_of(&entry, false).is_none() {
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
    let now = status_entries(root, Some(&manifest.parent), None)?;
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
        let counts =
            diff_counts(&sl(root, &["diff", "--git", "--no-binary", "-r", &manifest.parent])?);
        let renamed = rename_sources(&fresh);
        for entry in fresh {
            let source = entry.copy.as_deref().filter(|s| renamed.contains(s));
            let Some(kind) = kind_of(entry, source.is_some()) else { continue };
            if kind == ChangeKind::Deleted && renamed.contains(entry.path.as_str()) {
                continue;
            }
            // The baseline has no untracked concept: a file created during the turn is
            // an addition, matching git's tree diff.
            let kind = if kind == ChangeKind::Untracked { ChangeKind::Added } else { kind };
            let (additions, deletions) = match kind {
                ChangeKind::Added if entry.status == "?" || entry.copy.is_some() => {
                    (new_side_lines(root, None, &entry.path), 0)
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
                previous_path: source.map(str::to_string),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{
        Manifest, Pending, Store, abbreviate_node, cmdline, complete_pick_spelling, diff_counts,
        probe_revset, text_counts,
    };

    #[test]
    fn a_failed_command_names_itself_the_way_it_would_be_typed() {
        assert_eq!(cmdline(&["status", "-mardu", "-C", "-Tjson"]), "sl status -mardu -C -Tjson");
        // A template's tab and newline would otherwise break the status line in two.
        assert_eq!(
            cmdline(&["log", "-r", "draft() & ::.", "-T", "{node}\t{desc}\n"]),
            r"sl log -r 'draft() & ::.' -T '{node}\t{desc}\n'"
        );
    }

    #[test]
    fn probe_revset_fences_only_a_revision_number() {
        // Digit-only is a hash prefix at any length: bare, `123456` is a revnum and
        // would pin an ancient commit (`specs/sapling.md` Scopes).
        assert_eq!(probe_revset("123456"), "limit(present(id(123456)), 1)");
        assert_eq!(probe_revset("0"), "limit(present(id(0)), 1)");
        // Mixed hex goes bare, which resolves a hash prefix and a bookmark both. Under
        // `id()` a bookmark named `beef` resolves to nothing, and the picker lists it.
        assert_eq!(probe_revset("beef1234"), "limit(present(beef1234), 1)");
        assert_eq!(probe_revset("beef"), "limit(present(beef), 1)");
        assert_eq!(probe_revset("abc"), "limit(present(abc), 1)");
        assert_eq!(probe_revset("master"), "limit(present(master), 1)");
        // `present()` on every name: a bare unknown one that looks remote is pulled
        // before Sapling gives up on it, and reviewr writes nothing to the repository.
        assert_eq!(
            probe_revset("fbcode-nope"),
            "limit(present(fbcode-nope), 1)",
            "an unknown remote-shaped name must not reach the remote"
        );
    }

    #[test]
    fn a_node_paints_at_saplings_own_short_width() {
        // Twelve, not git's seven. A lazy monorepo clone resolves no prefix shorter than
        // seven at all, and calls a good share of seven ambiguous, so a node painted git's
        // way is one the reviewer cannot paste back into `sl`.
        let node = "76d14ba62b43b42bae519c1080a440d2428784db";
        assert_eq!(abbreviate_node(node), "76d14ba62b43");
        assert_eq!(abbreviate_node("76d14ba"), "76d14ba");
    }

    #[test]
    fn a_typed_hash_prefix_records_the_whole_node_and_a_name_records_itself() {
        let node = "2eb84b9c0d1e2f3a4b5c6d7e8f90112233445566";
        // The prefix resolved through `id()`, so recording it as typed leaves a pin the
        // monorepo can make ambiguous, and an ambiguous pick goes dormant.
        assert_eq!(complete_pick_spelling("2eb84b9", node), node);
        assert_eq!(complete_pick_spelling(node, node), node);
        // A name resolved as a name and keeps following its bookmark, even one that reads
        // as hex and even when its own commit happens to start with those letters.
        assert_eq!(complete_pick_spelling("main", node), "main");
        assert_eq!(complete_pick_spelling("2eb", "2eb84b9c0d1e"), "2eb");
        // A hex-shaped spelling that is not this oid's prefix is a name that resolved
        // elsewhere; it records as itself.
        assert_eq!(complete_pick_spelling("beefcafe", node), "beefcafe");
        assert_eq!(probe_revset("last(public() & ::.)"), "limit(present(last(public() & ::.)), 1)");
    }

    #[test]
    fn a_picked_commit_probes_through_its_successors() {
        // The recorded end reaches `successors` through the same bounded probe, so an
        // amended or rebased commit is followed whether the pick recorded a node or a name.
        assert_eq!(
            super::successor_revset("beef1234"),
            "limit(last(sort(successors(limit(present(beef1234), 1)) - obsolete(), rev)), 1)"
        );
        assert_eq!(
            super::successor_revset("123456"),
            "limit(last(sort(successors(limit(present(id(123456)), 1)) - obsolete(), rev)), 1)"
        );
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
    fn diff_counts_reads_a_path_whose_header_carries_a_trailing_tab() {
        // A path holding a space: `sl diff --git` separates the name from the header's
        // trailing field with a tab, and the key must still be the status path.
        let out = "diff --git a/old name.txt b/old name.txt\n\
                   --- a/old name.txt\t\n\
                   +++ b/old name.txt\t\n\
                   @@ -1,2 +1,2 @@\n \
                   spaced\n\
                   -content\n\
                   +edited\n";
        let m = diff_counts(out);
        assert_eq!(m["old name.txt"], (1, 1));
    }

    #[test]
    fn diff_counts_skips_a_binary_file_without_disturbing_its_neighbour() {
        // Under `--no-binary` a binary file is one prose line between two boundaries. It
        // must key nothing, and it must not fold its neighbour's hunk into itself.
        let out = "diff --git a/img.bin b/img.bin\n\
                   Binary file img.bin has changed\n\
                   diff --git a/t.txt b/t.txt\n\
                   --- a/t.txt\n\
                   +++ b/t.txt\n\
                   @@ -1,1 +1,2 @@\n \
                   hello\n\
                   +world\n";
        let m = diff_counts(out);
        assert!(!m.contains_key("img.bin"), "a binary file reads as (0, 0) by absence");
        assert_eq!(m["t.txt"], (1, 0));
    }

    #[test]
    fn text_counts_diff_lines_and_binary_is_zero() {
        assert_eq!(text_counts(b"a\nb\n", b"a\nc\nd\n"), (2, 1));
        assert_eq!(text_counts(b"", b"x\n"), (1, 0));
        assert_eq!(text_counts(b"bin\0", b"x"), (0, 0));
    }

    #[test]
    fn text_counts_refuses_a_pair_the_pane_would_not_render() {
        // 26k lines reordered: every line survives, so the differ explores a wide edit graph.
        // Ungated this pair takes 2.8s, and the cost is quadratic — 60k lines take 15s and
        // 120k take a minute. `last-turn` pays it on the world worker every poll, for a file
        // the pane shows `too_large` for either way.
        const N: usize = 26_000;
        let side = |order: fn(usize) -> usize| -> Vec<u8> {
            let mut out = Vec::new();
            for i in 0..N {
                out.extend_from_slice(format!("line {} filler content\n", order(i)).as_bytes());
            }
            out
        };
        let started = std::time::Instant::now();
        assert_eq!(text_counts(&side(|i| i), &side(|i| i * 7 % N)), (0, 0));
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "the gate did not hold");
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
        let base_only = super::Pick { base: "remote/main".into(), tip: None };
        assert_eq!(store.read_base_pick().unwrap(), Some(base_only));
        store.clear_base_pick().unwrap();
        assert_eq!(store.read_base_pick().unwrap(), None);
        store.write_base_pick("-bad").unwrap();
        assert_eq!(store.read_base_pick().unwrap(), None);
        // A commit pick records both of the range's ends (`specs/sapling.md` Scopes).
        store.write_base_pick("abc123^..abc123").unwrap();
        let commit = super::Pick { base: "abc123^".into(), tip: Some("abc123".into()) };
        assert_eq!(store.read_base_pick().unwrap(), Some(commit));
        store.clear_base_pick().unwrap();
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
        assert_eq!(
            store.read_blob("aa00"),
            Some(b"v0".to_vec()),
            "a blob written this minute outlives its own manifest"
        );
        assert_eq!(store.read_blob("aa05"), Some(b"v5".to_vec()));

        // Age every blob out of the grace window and sweep again: now the orphan goes and
        // the live baseline's blob stays.
        age_blobs(&store);
        store.prune(&ids[5]);
        assert_eq!(store.read_blob("aa00"), None, "a pruned manifest's blob goes with it");
        assert_eq!(store.read_blob("aa05"), Some(b"v5".to_vec()));
    }

    #[test]
    fn a_baseline_missing_a_blob_seeds_as_no_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_path_buf());
        let manifest = Manifest {
            parent: "p0".to_string(),
            files: BTreeMap::from([("f.rs".to_string(), Some("aa11".to_string()))]),
        };
        let id = manifest.id();
        let blobs = HashMap::from([("aa11".to_string(), std::sync::Arc::new(b"old".to_vec()))]);
        store.persist(&Pending { id: id.clone(), manifest, blobs }).unwrap();
        assert_eq!(store.read_baseline(), Some(id));

        // An older reviewr's sweep could take a live baseline's blob. Seeding that pointer
        // anyway costs an error on every `last-turn` build until the next turn promotes.
        std::fs::remove_file(store.blobs_dir().join("aa11")).unwrap();
        assert_eq!(store.read_baseline(), None);
    }

    #[test]
    fn an_unreadable_manifest_stops_the_blob_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_path_buf());
        let manifest = Manifest {
            parent: "p0".to_string(),
            files: BTreeMap::from([("f.rs".to_string(), Some("aa11".to_string()))]),
        };
        let id = manifest.id();
        let blobs = HashMap::from([("aa11".to_string(), std::sync::Arc::new(b"live".to_vec()))]);
        store.persist(&Pending { id: id.clone(), manifest, blobs }).unwrap();

        // A manifest that no longer parses still names live blobs. Sweeping on that
        // reading would delete the content of the very baseline the pointer names.
        std::fs::write(store.manifest_path(&id).unwrap(), b"{ truncated").unwrap();
        age_blobs(&store);
        store.prune(&id);
        assert_eq!(store.read_blob("aa11"), Some(b"live".to_vec()));
    }

    /// Backdate every blob past `WRITE_GRACE`, so a sweep judges it on references alone.
    fn age_blobs(store: &Store) {
        let old = std::time::SystemTime::now() - std::time::Duration::from_hours(1);
        for entry in std::fs::read_dir(store.blobs_dir()).unwrap() {
            let path = entry.unwrap().path();
            std::fs::File::options().write(true).open(path).unwrap().set_modified(old).unwrap();
        }
    }
}
