//! The VCS boundary: one kind per pane, dispatching every repository read to
//! `git` or `sl`.
//!
//! See `specs/sapling.md`. The kind is resolved once at pane open and fixed for the
//! pane's lifetime — a directory that becomes a Sapling repository after open needs a
//! reopen, while a git repository keeps the become-a-repo-later behavior `is_repo`
//! re-checks per build.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::git::{self, BaseStatus, GitFail, ResolvedBase, Worktree, WorktreeEntry};
use crate::model::{ChangedFile, Scope};
use crate::sl;

/// Which VCS the reviewed worktree speaks. A root both git and Sapling claim is
/// reviewed as git (`specs/sapling.md`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VcsKind {
    #[default]
    Git,
    Sapling,
}

/// Resolve `path` to its worktree root and kind: the git top level first, then
/// `sl root`, else the path itself as a (possibly not-yet-a-repo) git target
/// (`specs/sapling.md`).
pub fn resolve_repo(path: &Path) -> (PathBuf, VcsKind) {
    if let Worktree::Root(root) = git::worktree_of(path) {
        return (root, VcsKind::Git);
    }
    if let Worktree::Root(root) = sl::root_of(path) {
        return (root, VcsKind::Sapling);
    }
    (path.to_path_buf(), VcsKind::Git)
}

/// The kind at an already-resolved root, from its marker directory alone — no
/// subprocess, so App construction and every world build stay cheap. `.git` wins a
/// dual-marker root (`specs/sapling.md`).
pub fn kind_at(root: &Path) -> VcsKind {
    if root.join(".git").exists() {
        return VcsKind::Git;
    }
    if root.join(".sl").exists() || root.join(".hg").exists() {
        return VcsKind::Sapling;
    }
    VcsKind::Git
}

/// Whether `root` currently is a repository of `kind`. The git arm keeps the
/// subprocess probe so a directory that becomes a repo mid-session starts showing
/// changes; the Sapling arm is a marker check, since a Sapling pane's kind was proven
/// at open (`specs/sapling.md`).
pub fn is_repo(kind: VcsKind, root: &Path) -> bool {
    match kind {
        VcsKind::Git => git::is_repo(root),
        VcsKind::Sapling => root.join(".sl").exists() || root.join(".hg").exists(),
    }
}

/// Resolve a directory to its worktree root under `kind` — agent membership for the
/// turn fold (`specs/herdr-host.md`, `specs/sapling.md`).
pub fn worktree_of(kind: VcsKind, path: &Path) -> Worktree {
    match kind {
        VcsKind::Git => git::worktree_of(path),
        VcsKind::Sapling => sl::root_of(path),
    }
}

/// The `branch` scope's two ends: the resolved base the changeset diffs from, and the
/// commit its far end sits on. A `None` tip ends the range at the working copy, which is
/// every git range and every Sapling range but a commit pick (`specs/sapling.md` Scopes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchEnds {
    pub base: BaseStatus,
    pub tip: Option<Tip>,
}

/// The commit a range's far end sits on. The title rides the node so the header cannot
/// name one commit and describe another (`specs/sapling.md` Scopes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tip {
    pub oid: String,
    /// The commit's code review number, empty when it carries none.
    pub diff: String,
    /// The commit description's first line, empty when the commit has none.
    pub title: String,
}

/// The changed files for `scope` (`specs/review-model.md` Scopes, `specs/sapling.md`).
pub fn changed_files(
    kind: VcsKind,
    root: &Path,
    scope: Scope,
    branch_base: Option<&str>,
    branch_tip: Option<&str>,
) -> Result<Vec<ChangedFile>> {
    match kind {
        VcsKind::Git => git::changed_files(root, scope, branch_base),
        VcsKind::Sapling => sl::changed_files(root, scope, branch_base, branch_tip),
    }
}

/// The changed files between the turn baseline and the live worktree
/// (`specs/review-model.md` Turn baseline).
pub fn changed_against_tree(kind: VcsKind, root: &Path, tree: &str) -> Result<Vec<ChangedFile>> {
    match kind {
        VcsKind::Git => git::changed_against_tree(root, tree),
        VcsKind::Sapling => sl::changed_against_snapshot(root, tree),
    }
}

/// The content of `path` at `rev`; empty when the path does not exist there. `rev` may
/// be a turn-baseline id: a git tree object, or a Sapling `snap:` manifest
/// (`specs/sapling.md` The snapshot store).
pub fn file_content(kind: VcsKind, root: &Path, rev: &str, path: &str) -> String {
    match kind {
        VcsKind::Git => git::file_content(root, rev, path),
        VcsKind::Sapling => sl::file_content(root, rev, path),
    }
}

/// The revision the `uncommitted` scope's old side reads: `HEAD`, or the working-copy
/// parent (`specs/sapling.md`).
pub fn uncommitted_base(kind: VcsKind) -> &'static str {
    match kind {
        VcsKind::Git => "HEAD",
        VcsKind::Sapling => ".",
    }
}

/// Read one file's old side into the backend's content cache ahead of the reviewer opening
/// it. Git reads a blob in single-digit milliseconds, so it warms nothing
/// (`specs/sapling.md` Reads).
pub fn warm_content(kind: VcsKind, root: &Path, rev: &str, path: &str) {
    match kind {
        VcsKind::Git => (),
        VcsKind::Sapling => sl::warm_content(root, rev, path),
    }
}

/// Whether a warm or preload has already landed `path` at `rev` in the backend's content cache.
/// Git caches nothing, so it is never holding anything ([`warm_content`]).
pub fn content_is_cached(kind: VcsKind, rev: &str, path: &str) -> bool {
    match kind {
        VcsKind::Git => false,
        VcsKind::Sapling => sl::content_is_cached(rev, path),
    }
}

/// Read one file into the backend's content cache and wait for it. The world worker calls
/// this before handing over a snapshot that moved the revisions under the open file, so the
/// landing frame finds a cache hit where it would otherwise spawn `sl cat`. Git warms
/// nothing, on the same reasoning as [`warm_content`].
pub fn preload_content(kind: VcsKind, root: &Path, rev: &str, path: &str) {
    match kind {
        VcsKind::Git => (),
        VcsKind::Sapling => {
            sl::file_content(root, rev, path);
        }
    }
}

/// The revisions the open file's two sides read at, in the order old then new. A `None` old
/// side is a scope with no base to read from. A `None` new side is the worktree, which is
/// every side but a `branch` range whose far end a commit pick pinned (`specs/sapling.md`
/// Scopes).
///
/// One derivation for the diff build, the neighbour warm, and the world worker's preload, so
/// none of them can fill or await a key another will not ask for.
pub fn read_revs(
    kind: VcsKind,
    root: &Path,
    scope: Scope,
    branch: &BranchEnds,
    turn_baseline: Option<&str>,
) -> (Option<String>, Option<String>) {
    let old = match scope {
        Scope::Uncommitted => Some(uncommitted_base(kind).to_string()),
        Scope::Branch => match branch.base.winner.as_ref().map(git::ResolvedBase::oid) {
            // A pinned far end's base is that commit's own parent, so no merge base stands
            // between the two.
            Some(base) if branch.tip.is_some() => Some(base.to_string()),
            Some(base) => merge_base(kind, root, base),
            None => None,
        },
        Scope::LastTurn => turn_baseline.map(str::to_string),
    };
    let new = branch.tip.as_ref().filter(|_| scope == Scope::Branch).map(|t| t.oid.clone());
    (old, new)
}

/// The merge-base commit of the resolved base and the working copy
/// (`specs/review-model.md` Base branch).
pub fn merge_base(kind: VcsKind, root: &Path, base_oid: &str) -> Option<String> {
    match kind {
        VcsKind::Git => git::merge_base(root, base_oid),
        VcsKind::Sapling => sl::merge_base(root, base_oid),
    }
}

/// Resolve the base chain to the outcome the header paints (`specs/review-model.md`
/// Base branch, `specs/sapling.md` Scopes).
pub fn resolve_base(
    kind: VcsKind,
    root: &Path,
    base_flag: Option<&str>,
) -> Result<BranchEnds, GitFail> {
    match kind {
        VcsKind::Git => {
            git::resolve_base(root, base_flag).map(|r| BranchEnds { base: r.status, tip: None })
        }
        VcsKind::Sapling => sl::resolve_base(root, base_flag),
    }
}

/// Resolve one typed base-picker spelling (`specs/input.md` Base picker).
pub fn resolve_spelling(
    kind: VcsKind,
    root: &Path,
    spelling: &str,
) -> Result<Option<ResolvedBase>, GitFail> {
    match kind {
        VcsKind::Git => git::resolve_spelling(root, spelling),
        VcsKind::Sapling => sl::resolve_spelling(root, spelling),
    }
}

/// The default-branch name the picker marks; a Sapling repository has none, so its pick
/// can only be replaced, never cleared (`specs/sapling.md`).
pub fn default_branch_name(kind: VcsKind, root: &Path) -> Result<Option<String>, GitFail> {
    match kind {
        VcsKind::Git => git::default_branch_name(root),
        VcsKind::Sapling => Ok(None),
    }
}

/// The names the base picker lists: branches, or local bookmarks
/// (`specs/sapling.md` Scopes).
pub fn list_branches(
    kind: VcsKind,
    root: &Path,
    default: Option<&str>,
) -> Result<Vec<String>, GitFail> {
    match kind {
        VcsKind::Git => git::list_branches(root, default),
        VcsKind::Sapling => sl::list_bookmarks(root),
    }
}

/// The stack commits the base picker lists below the names, each a commit to review
/// rather than a base. A git repository lists none: recent commits are typed, not offered
/// (`specs/input.md` Non-goals).
pub fn list_stack(kind: VcsKind, root: &Path) -> Result<Vec<sl::StackCommit>, GitFail> {
    match kind {
        VcsKind::Git => Ok(Vec::new()),
        VcsKind::Sapling => sl::list_stack(root),
    }
}

/// Record a typed pick's SHA spelling: git completes a unique prefix to the abbreviation,
/// Sapling to the whole node — a 7-hex prefix is routinely ambiguous in a monorepo, and an
/// ambiguous pick goes dormant (`specs/review-model.md` Base branch). The public-base
/// spelling already records whole for that reason.
#[must_use]
pub fn complete_pick_spelling(kind: VcsKind, spelling: &str, oid: &str) -> String {
    match kind {
        VcsKind::Git => git::complete_sha_prefix(spelling, oid),
        VcsKind::Sapling => sl::complete_pick_spelling(spelling, oid),
    }
}

/// The abbreviated commit id the header and the base picker paint: git's seven hex, or
/// Sapling's twelve (`specs/sapling.md` Scopes).
#[must_use]
pub fn abbreviate_oid(kind: VcsKind, oid: &str) -> String {
    match kind {
        VcsKind::Git => git::abbreviate_oid(oid),
        VcsKind::Sapling => sl::abbreviate_node(oid),
    }
}

/// Shown name and optional abbreviated commit id for a non-branch spelling
/// (`specs/tui.md`). A hash prefix paints once; anything else keeps the spelling and
/// carries the mark. Prefix-shaped is one rule for both kinds, since only the width the
/// mark paints at differs.
#[must_use]
pub fn rev_paint(kind: VcsKind, spelling: &str, oid: &str) -> (String, Option<String>) {
    let abbrev = abbreviate_oid(kind, oid);
    if git::spelling_is_sha_prefix(spelling, oid) {
        (abbrev, None)
    } else {
        (spelling.to_string(), Some(abbrev))
    }
}

/// Persist the base pick: a private ref, or the snapshot store
/// (`specs/sapling.md` SL-NO-REPO-WRITES).
pub fn write_base_pick(kind: VcsKind, root: &Path, name: &str) -> Result<(), GitFail> {
    match kind {
        VcsKind::Git => git::write_base_pick(root, name),
        VcsKind::Sapling => sl::Store::open(root).write_base_pick(name),
    }
}

/// Whether `spelling` won the base chain as the chain's own fallback rather than as a
/// recorded pick. The base picker offers a row for a pick no list row names, and never
/// for the fallback, which the default row already selects (`specs/sapling.md` Scopes).
///
/// Git's fallback is a branch, so a rev winner there is always a pick and the answer
/// needs no read. Sapling's fallback is the public base, a rev like any pinned one, so it
/// reads the store. That read is a file, never a spawn. A failed read answers `false`,
/// keeping the row rather than hiding the reviewer's own pick.
pub fn base_is_chain_fallback(kind: VcsKind, root: &Path, spelling: &str) -> bool {
    match kind {
        VcsKind::Git => false,
        VcsKind::Sapling => match sl::Store::open(root).read_base_pick() {
            Ok(pick) => pick.is_none_or(|p| p.base != spelling),
            Err(_) => false,
        },
    }
}

/// Drop the recorded pick (`specs/review-model.md` Base branch).
pub fn clear_base_pick(kind: VcsKind, root: &Path) -> Result<(), GitFail> {
    match kind {
        VcsKind::Git => git::clear_base_pick(root),
        VcsKind::Sapling => sl::Store::open(root).clear_base_pick(),
    }
}

/// The persisted turn baseline for `root`, if any (`specs/herdr-host.md`,
/// `specs/sapling.md` The snapshot store).
pub fn seed_baseline(kind: VcsKind, root: &Path) -> Option<String> {
    match kind {
        VcsKind::Git => git::read_baseline_ref(root, &git::worktree_key(root)),
        VcsKind::Sapling => sl::Store::open(root).read_baseline(),
    }
}

/// The `All files` worktree listing. A Sapling worktree lists nothing: enumerating a
/// monorepo violates `SL-SCALE-CHANGED` (`specs/sapling.md` Disabled surfaces).
pub fn all_files(kind: VcsKind, root: &Path) -> Result<Vec<WorktreeEntry>> {
    match kind {
        VcsKind::Git => git::all_files(root),
        VcsKind::Sapling => Ok(Vec::new()),
    }
}

/// Addition count of a worktree file read locally: its line count, 0 for empty or
/// binary — what a diff against empty old content reports. Shared by both backends'
/// untracked passes.
pub(crate) fn line_additions(root: &Path, path: &str) -> u32 {
    let Ok(bytes) = std::fs::read(root.join(path)) else { return 0 };
    text_line_count(&bytes)
}

/// Lines in `bytes`: newline count plus an unterminated final line; 0 for empty or
/// binary (a NUL byte), matching git's numstat.
pub(crate) fn text_line_count(bytes: &[u8]) -> u32 {
    if bytes.is_empty() || bytes.contains(&0) {
        return 0;
    }
    #[allow(clippy::naive_bytecount)]
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count();
    let trailing = usize::from(bytes.last() != Some(&b'\n'));
    (newlines + trailing) as u32
}

#[cfg(test)]
mod tests {
    use super::{VcsKind, kind_at, text_line_count};

    #[test]
    fn kind_at_reads_markers_and_git_wins_a_dual_root() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(kind_at(dir.path()), VcsKind::Git, "no marker defaults to git");
        std::fs::create_dir(dir.path().join(".hg")).unwrap();
        assert_eq!(kind_at(dir.path()), VcsKind::Sapling);
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(kind_at(dir.path()), VcsKind::Git, "git claims a dual-marker root");
        let sl_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(sl_dir.path().join(".sl")).unwrap();
        assert_eq!(kind_at(sl_dir.path()), VcsKind::Sapling);
    }

    #[test]
    fn line_count_matches_numstat_semantics() {
        assert_eq!(text_line_count(b""), 0);
        assert_eq!(text_line_count(b"one\n"), 1);
        assert_eq!(text_line_count(b"one\ntwo"), 2);
        assert_eq!(text_line_count(b"bin\0ary"), 0);
    }
}
