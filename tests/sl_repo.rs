//! The Sapling backend contract (`specs/sapling.md`), against real `sl` repos in temp
//! dirs. Every test skips quietly when `sl` is not installed, so the suite stays green
//! on machines without Sapling.

use std::path::{Path, PathBuf};
use std::process::Command;

use herdr_reviewr::model::{ChangeKind, Scope};
use herdr_reviewr::vcs::{self, VcsKind};

/// A throwaway Sapling repository, or `None` when `sl` is unavailable.
struct SlRepo {
    dir: tempfile::TempDir,
}

impl SlRepo {
    fn init() -> Option<Self> {
        let dir = tempfile::tempdir().unwrap();
        let ok = Command::new("sl")
            .current_dir(dir.path())
            .env("HGPLAIN", "1")
            .arg("init")
            .output()
            .is_ok_and(|o| o.status.success());
        ok.then_some(Self { dir })
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The canonical root, matching what `sl root` prints (a macOS or /tmp symlink
    /// otherwise keys the store and the app apart).
    fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.path()).unwrap()
    }

    fn sl(&self, args: &[&str]) -> String {
        let out = Command::new("sl")
            .current_dir(self.path())
            .env("HGPLAIN", "1")
            .args(["--config", "ui.username=Test <test@herdr.test>"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "sl {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn commit_all(&self, msg: &str) {
        self.sl(&["addremove", "-q"]);
        self.sl(&["commit", "-m", msg]);
    }

    fn parent(&self) -> String {
        self.sl(&["whereami"]).lines().next().unwrap().trim().to_string()
    }
}

/// Removes a worktree's snapshot-store directory on drop, so a test that persisted a
/// baseline through the default store resolution leaves no state behind.
struct StoreGuard(PathBuf);

impl StoreGuard {
    fn for_root(root: &Path) -> Self {
        Self(herdr_reviewr::sl::Store::open(root).dir().to_path_buf())
    }
}

impl Drop for StoreGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

macro_rules! sl_repo_or_skip {
    () => {
        match SlRepo::init() {
            Some(repo) => repo,
            None => {
                eprintln!("skipping: sl is not installed");
                return;
            }
        }
    };
}

#[test]
fn resolve_repo_finds_the_sl_root_and_kind_from_a_subdir() {
    let r = sl_repo_or_skip!();
    r.write("sub/dir/f.txt", "x\n");
    let (root, kind) = vcs::resolve_repo(&r.path().join("sub/dir"));
    assert_eq!(kind, VcsKind::Sapling);
    assert_eq!(root, r.root());
    assert_eq!(vcs::kind_at(&root), VcsKind::Sapling);
    assert!(vcs::is_repo(VcsKind::Sapling, &root));
}

#[test]
fn uncommitted_scope_lists_kinds_and_counts() {
    let r = sl_repo_or_skip!();
    r.write("a.txt", "one\ntwo\n");
    r.write("b.txt", "gone\n");
    r.write("d.txt", "x\ny\nz\n");
    r.commit_all("base");
    r.write("a.txt", "one\ntwo\nthree\n");
    r.sl(&["rm", "b.txt"]);
    r.write("c.txt", "1\n2\n3\n");
    // A plain `rm` (status `!`, never `sl rm`'d) is how agents delete files.
    std::fs::remove_file(r.path().join("d.txt")).unwrap();
    let files = vcs::changed_files(VcsKind::Sapling, &r.root(), Scope::Uncommitted, None).unwrap();
    let by_path = |p: &str| files.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("{p}"));
    assert_eq!(by_path("a.txt").kind, ChangeKind::Modified);
    assert_eq!((by_path("a.txt").additions, by_path("a.txt").deletions), (1, 0));
    assert_eq!(by_path("b.txt").kind, ChangeKind::Deleted);
    assert_eq!(by_path("c.txt").kind, ChangeKind::Untracked);
    assert_eq!(by_path("c.txt").additions, 3);
    assert_eq!(by_path("d.txt").kind, ChangeKind::Deleted);
    assert_eq!(by_path("d.txt").deletions, 3, "a plain-rm deletion still counts its old side");
    assert_eq!(files.len(), 4);
}

#[test]
fn a_digit_only_spelling_never_resolves_as_a_local_revision_number() {
    let r = sl_repo_or_skip!();
    r.write("a.txt", "a\n");
    r.commit_all("first");
    r.write("a.txt", "b\n");
    r.commit_all("second");
    // Bare in a revset, `0` is local revision zero — resolving it would let a pasted
    // digit-only prefix pin an arbitrary ancient commit (`specs/sapling.md` Scopes).
    // Under prefix semantics a hit is possible only when some hash starts with `0`,
    // so any answer must be such a hash, never the revnum-0 commit by number.
    if let Some(hit) = vcs::resolve_spelling(VcsKind::Sapling, &r.root(), "0").unwrap() {
        assert!(hit.oid().starts_with('0'), "digit-only resolved as a revnum: {hit:?}");
    }
    // A real hash prefix still resolves.
    let parent = r.parent();
    let hit = vcs::resolve_spelling(VcsKind::Sapling, &r.root(), &parent[..12]).unwrap();
    assert_eq!(hit.expect("a unique prefix resolves").oid(), parent);
}

#[test]
fn a_rename_carries_its_previous_path() {
    let r = sl_repo_or_skip!();
    r.write("old.txt", "keep this line\n");
    r.commit_all("base");
    r.sl(&["mv", "old.txt", "new.txt"]);
    let files = vcs::changed_files(VcsKind::Sapling, &r.root(), Scope::Uncommitted, None).unwrap();
    let renamed = files.iter().find(|f| f.path == "new.txt").expect("the rename lists");
    assert_eq!(renamed.kind, ChangeKind::Renamed);
    assert_eq!(renamed.previous_path.as_deref(), Some("old.txt"));
    // The move's `R old.txt` record folds into the rename, matching git's single row.
    assert_eq!(files.len(), 1, "{files:?}");
}

#[test]
fn branch_scope_diffs_committed_and_dirty_work_against_a_flag_base() {
    let r = sl_repo_or_skip!();
    r.write("a.txt", "a\n");
    r.write("b.txt", "b\n");
    r.commit_all("base");
    let base = r.parent();
    r.write("a.txt", "a\nchanged in commit\n");
    r.commit_all("stack commit");
    r.write("b.txt", "b\ndirty edit\n");
    let status = vcs::resolve_base(VcsKind::Sapling, &r.root(), Some(&base)).unwrap();
    let winner = status.winner.expect("the flag resolves");
    assert_eq!(winner.oid(), base);
    let files =
        vcs::changed_files(VcsKind::Sapling, &r.root(), Scope::Branch, Some(winner.oid())).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["a.txt", "b.txt"], "committed and dirty changes both list");
    assert!(files.iter().all(|f| f.kind == ChangeKind::Modified));
    assert_eq!(files[0].additions, 1);
}

#[test]
fn a_repo_with_no_public_commits_has_no_default_base() {
    let r = sl_repo_or_skip!();
    r.write("a.txt", "a\n");
    r.commit_all("draft only");
    let status = vcs::resolve_base(VcsKind::Sapling, &r.root(), None).unwrap();
    assert!(status.winner.is_none(), "an empty public() is a no-base state, not an error");
    assert!(status.skipped.is_none());
}

#[test]
fn an_unknown_flag_spelling_is_skipped_never_an_error() {
    let r = sl_repo_or_skip!();
    r.write("a.txt", "a\n");
    r.commit_all("base");
    let status = vcs::resolve_base(VcsKind::Sapling, &r.root(), Some("no-such-name")).unwrap();
    assert!(status.winner.is_none());
    assert_eq!(status.skipped.as_deref(), Some("no-such-name"));
}

#[test]
fn file_content_reads_at_a_rev_and_absence_is_empty() {
    let r = sl_repo_or_skip!();
    r.write("a.txt", "committed content\n");
    r.commit_all("base");
    let rev = r.parent();
    r.write("a.txt", "worktree content\n");
    assert_eq!(
        vcs::file_content(VcsKind::Sapling, &r.root(), &rev, "a.txt"),
        "committed content\n"
    );
    assert_eq!(vcs::file_content(VcsKind::Sapling, &r.root(), &rev, "nope.txt"), "");
    // The `uncommitted` old side pins `.` to the parent before reading.
    assert_eq!(
        vcs::file_content(
            VcsKind::Sapling,
            &r.root(),
            vcs::uncommitted_base(VcsKind::Sapling),
            "a.txt"
        ),
        "committed content\n"
    );
}

#[test]
fn a_turn_snapshot_promotes_and_diffs_against_the_store() {
    let r = sl_repo_or_skip!();
    let root = r.root();
    let _guard = StoreGuard::for_root(&root);
    r.write("a.txt", "one\n");
    r.write("clean.txt", "clean\n");
    r.commit_all("base");
    // The turn starts with a.txt already dirty: its bytes at the turn start are the
    // baseline side, stored as a blob.
    r.write("a.txt", "one\ntwo\n");
    let mut turn = herdr_reviewr::sl::TurnStore::open(root.clone());
    let candidate = turn.snapshot().unwrap();
    turn.pin_candidate(&candidate);
    // The turn edits a dirty file and a clean one; the divergence check promotes.
    r.write("a.txt", "one\ntwo\nthree\n");
    r.write("clean.txt", "clean\nedited\n");
    let now = turn.snapshot().unwrap();
    assert_ne!(now, candidate, "the digest moves with the worktree");
    turn.persist_baseline(&candidate).unwrap();
    assert_eq!(turn.read_baseline(), Some(candidate.clone()));
    // The last-turn changeset: both edits, counted against the snapshot bytes.
    let files = vcs::changed_against_tree(VcsKind::Sapling, &root, &candidate).unwrap();
    let by_path = |p: &str| files.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("{p}"));
    assert_eq!(files.len(), 2);
    assert_eq!(by_path("a.txt").kind, ChangeKind::Modified);
    assert_eq!((by_path("a.txt").additions, by_path("a.txt").deletions), (1, 0));
    assert_eq!(by_path("clean.txt").kind, ChangeKind::Modified);
    // Old sides: the dirty file reads its stored blob, the clean file reads the parent.
    assert_eq!(vcs::file_content(VcsKind::Sapling, &root, &candidate, "a.txt"), "one\ntwo\n");
    assert_eq!(vcs::file_content(VcsKind::Sapling, &root, &candidate, "clean.txt"), "clean\n");
    // A restart seeds the persisted baseline back.
    assert_eq!(vcs::seed_baseline(VcsKind::Sapling, &root), Some(candidate));
}

#[test]
fn the_app_opens_a_sapling_repo_and_lists_its_changes() {
    let r = sl_repo_or_skip!();
    r.write("src/a.rs", "fn a() {}\n");
    r.commit_all("base");
    r.write("src/a.rs", "fn a() { /* edited */ }\n");
    let mut app = herdr_reviewr::app::App::new(r.root(), Scope::Uncommitted, None);
    assert_eq!(app.vcs, VcsKind::Sapling);
    app.reload().unwrap();
    assert_eq!(app.entries.len(), 1);
    assert_eq!(app.entries[0].path, "src/a.rs");
}
