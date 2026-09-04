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

/// Paint one frame through ratatui's test backend, like `tests/render.rs`.
fn painted(app: &herdr_reviewr::app::App) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 20)).unwrap();
    terminal.draw(|f| herdr_reviewr::ui::render(f, app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer.cell((x, y)).map_or(" ", ratatui::buffer::Cell::symbol));
        }
        out.push('\n');
    }
    out
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
    let files =
        vcs::changed_files(VcsKind::Sapling, &r.root(), Scope::Uncommitted, None, None).unwrap();
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
    let files =
        vcs::changed_files(VcsKind::Sapling, &r.root(), Scope::Uncommitted, None, None).unwrap();
    let renamed = files.iter().find(|f| f.path == "new.txt").expect("the rename lists");
    assert_eq!(renamed.kind, ChangeKind::Renamed);
    assert_eq!(renamed.previous_path.as_deref(), Some("old.txt"));
    // The move's `R old.txt` record folds into the rename, matching git's single row.
    assert_eq!(files.len(), 1, "{files:?}");
}

#[test]
fn a_copy_reviews_as_an_added_file_because_its_source_stays() {
    let r = sl_repo_or_skip!();
    r.write("orig.txt", "one\ntwo\n");
    r.commit_all("base");
    r.sl(&["cp", "orig.txt", "copy.txt"]);
    r.write("copy.txt", "one\ntwo\nthree\n");
    let files =
        vcs::changed_files(VcsKind::Sapling, &r.root(), Scope::Uncommitted, None, None).unwrap();

    assert_eq!(files.len(), 1, "orig.txt is untouched, so only the copy lists: {files:?}");
    let copy = &files[0];
    assert_eq!(copy.path, "copy.txt");
    assert_eq!(
        copy.kind,
        ChangeKind::Added,
        "naming a live file as the source would read as a move"
    );
    assert_eq!(copy.previous_path, None);
    // `sl diff` measures the copy against its source, +1. The whole new file is the addition.
    assert_eq!((copy.additions, copy.deletions), (3, 0));
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
    let winner = status.base.winner.expect("the flag resolves");
    assert_eq!(winner.oid(), base);
    let files =
        vcs::changed_files(VcsKind::Sapling, &r.root(), Scope::Branch, Some(winner.oid()), None)
            .unwrap();
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
    assert!(status.base.winner.is_none(), "an empty public() is a no-base state, not an error");
    assert!(status.base.skipped.is_none());
}

#[test]
fn an_unknown_flag_spelling_is_skipped_never_an_error() {
    let r = sl_repo_or_skip!();
    r.write("a.txt", "a\n");
    r.commit_all("base");
    let status = vcs::resolve_base(VcsKind::Sapling, &r.root(), Some("no-such-name")).unwrap();
    assert!(status.base.winner.is_none());
    assert_eq!(status.base.skipped.as_deref(), Some("no-such-name"));
}

#[test]
fn a_dormant_pick_names_its_node_abbreviated() {
    let r = sl_repo_or_skip!();
    r.write("a.txt", "a\n");
    r.commit_all("base");
    let root = r.root();
    let _guard = StoreGuard::for_root(&root);
    // A pick records all 40 hex digits, so the whole node would fill the header.
    let gone = "0123456789abcdef0123456789abcdef01234567";
    vcs::write_base_pick(VcsKind::Sapling, &root, &format!("{gone}^..{gone}")).unwrap();
    let mut app = herdr_reviewr::app::App::new(root.clone(), Scope::Branch, None);
    app.reload().unwrap();

    let painted = painted(&app);
    assert!(painted.contains(&format!("{} missing", &gone[..7])), "{painted}");
    assert!(!painted.contains(gone), "the whole node never paints");
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

/// Three commits, each adding one file of its own, plus one uncommitted edit.
fn stacked_repo(r: &SlRepo) {
    for msg in ["first commit", "second commit", "third commit"] {
        r.write(&format!("{}.txt", msg.split(' ').next().unwrap()), "committed\n");
        r.commit_all(msg);
    }
    r.write("dirty.txt", "uncommitted\n");
}

#[test]
fn a_commit_pick_reviews_that_commit_against_its_own_parent() {
    let r = sl_repo_or_skip!();
    stacked_repo(&r);
    let root = r.root();
    let _guard = StoreGuard::for_root(&root);
    // `.^` is the second commit, so the range under review is `second^ → second`.
    let node = r.sl(&["log", "-r", ".^", "-T", "{node}"]);
    let parent = r.sl(&["log", "-r", ".^^", "-T", "{node}"]);
    vcs::write_base_pick(VcsKind::Sapling, &root, &format!("{node}^..{node}")).unwrap();
    let mut app = herdr_reviewr::app::App::new(root.clone(), Scope::Branch, None);
    app.reload().unwrap();

    assert_eq!(
        app.branch_tip.as_ref().map(|t| t.oid.as_str()),
        Some(node.as_str()),
        "the pick pins the far end"
    );
    assert_eq!(
        app.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        ["second.txt"],
        "one commit's own changes: not the commit above it, not the uncommitted edit"
    );
    let want = format!("vs {} → {} · second commit", &parent[..7], &node[..7]);
    assert!(
        painted(&app).contains(&want),
        "the header names both ends and the commit; wanted {want:?}"
    );
}

#[test]
fn a_commit_pick_follows_the_commit_through_an_amend() {
    let r = sl_repo_or_skip!();
    for msg in ["first commit", "second commit"] {
        r.write(&format!("{}.txt", msg.split(' ').next().unwrap()), "committed\n");
        r.commit_all(msg);
    }
    let root = r.root();
    let _guard = StoreGuard::for_root(&root);
    let node = r.sl(&["log", "-r", ".", "-T", "{node}"]);
    let parent = r.sl(&["log", "-r", ".^", "-T", "{node}"]);
    vcs::write_base_pick(VcsKind::Sapling, &root, &format!("{node}^..{node}")).unwrap();

    // The reviewed commit is where the agent applies its fix, and `amend` replaces its node.
    r.write("second.txt", "committed\namended\n");
    r.write("extra.txt", "the amend added this\n");
    r.sl(&["addremove", "-q"]);
    r.sl(&["amend"]);
    let amended = r.sl(&["log", "-r", ".", "-T", "{node}"]);
    assert_ne!(amended, node, "the amend replaces the node");

    let mut app = herdr_reviewr::app::App::new(root.clone(), Scope::Branch, None);
    app.reload().unwrap();

    assert_eq!(
        app.branch_tip.as_ref().map(|t| t.oid.as_str()),
        Some(amended.as_str()),
        "the pick follows its successor"
    );
    let mut paths = app.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>();
    paths.sort_unstable();
    assert_eq!(paths, ["extra.txt", "second.txt"], "the amended content is what is under review");
    let want = format!("vs {} → {}", &parent[..7], &amended[..7]);
    assert!(painted(&app).contains(&want), "the header names the new node; wanted {want:?}");
}

#[test]
fn a_pick_on_the_root_commit_is_skipped_for_want_of_a_parent() {
    let r = sl_repo_or_skip!();
    r.write("a.txt", "one\n");
    r.commit_all("the root commit");
    let root = r.root();
    let _guard = StoreGuard::for_root(&root);
    let node = r.sl(&["log", "-r", ".", "-T", "{node}"]);
    vcs::write_base_pick(VcsKind::Sapling, &root, &format!("{node}^..{node}")).unwrap();

    // A root commit's parent is the null node, so the range has no far end to diff from.
    let ends = vcs::resolve_base(VcsKind::Sapling, &root, None).unwrap();
    assert_eq!(ends.tip, None);
    assert_eq!(ends.base.winner, None, "the repo has no public commit to fall back to");
    assert_eq!(
        ends.base.skipped.as_deref(),
        Some(&node[..7]),
        "the pick reports as skipped, named by its abbreviated node"
    );
}

/// Captures the export payload, so a test can read what the agent would have been sent.
#[derive(Default)]
struct Captured(std::cell::RefCell<String>);

impl herdr_reviewr::export::ExportTarget for Captured {
    fn label(&self) -> &'static str {
        "captured"
    }
    fn success_message(&self, count: usize) -> String {
        format!("exported {count}")
    }
    fn failure_message(&self) -> String {
        "unreachable".to_string()
    }
    fn export(&self, text: &str) -> anyhow::Result<()> {
        self.0.replace(text.to_string());
        Ok(())
    }
}

#[test]
fn a_commit_pick_names_the_commit_in_the_export_preamble() {
    let r = sl_repo_or_skip!();
    stacked_repo(&r);
    let root = r.root();
    let _guard = StoreGuard::for_root(&root);
    let node = r.sl(&["log", "-r", ".^", "-T", "{node}"]);
    vcs::write_base_pick(VcsKind::Sapling, &root, &format!("{node}^..{node}")).unwrap();
    let mut app = herdr_reviewr::app::App::new(root.clone(), Scope::Branch, None);
    app.reload().unwrap();

    // A comment on the reviewed commit's own added line.
    app.focus = herdr_reviewr::app::Focus::Diff;
    app.diff_cursor =
        app.diff.rows.iter().position(|row| row.marker() == '+').expect("an added row");
    app.start_comment();
    for ch in "revert this".chars() {
        app.input_push(ch);
    }
    app.submit_comment();
    assert_eq!(app.store.len(), 1);

    let target = Captured::default();
    assert!(app.export(&target));
    let sent = target.0.borrow().clone();
    let want = format!("reviewing commit {}, not the working copy", &node[..7]);
    assert_eq!(
        sent.lines().next().unwrap(),
        want,
        "the export leads with the commit under review, since the worktree is not it"
    );
    assert!(sent.contains("\n\nsecond.txt:1\n"), "the comment block follows it: {sent:?}");
}

#[test]
fn an_uncommitted_send_names_no_commit_even_while_a_pick_stands() {
    let r = sl_repo_or_skip!();
    stacked_repo(&r);
    let root = r.root();
    let _guard = StoreGuard::for_root(&root);
    let node = r.sl(&["log", "-r", ".^", "-T", "{node}"]);
    vcs::write_base_pick(VcsKind::Sapling, &root, &format!("{node}^..{node}")).unwrap();
    let mut app = herdr_reviewr::app::App::new(root.clone(), Scope::Uncommitted, None);
    app.reload().unwrap();

    app.focus = herdr_reviewr::app::Focus::Diff;
    app.diff_cursor =
        app.diff.rows.iter().position(|row| row.marker() == '+').expect("an added row");
    app.start_comment();
    app.input_push('x');
    app.submit_comment();

    let target = Captured::default();
    assert!(app.export(&target));
    assert!(
        target.0.borrow().starts_with("dirty.txt:"),
        "the active scope reviews the worktree, so nothing is named: {:?}",
        target.0.borrow()
    );
}

#[test]
fn the_stack_lists_the_draft_commits_connected_to_the_working_copy() {
    let r = sl_repo_or_skip!();
    stacked_repo(&r);
    let stack = vcs::list_stack(VcsKind::Sapling, &r.root()).unwrap();
    assert_eq!(
        stack.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        ["third commit", "second commit", "first commit"],
        "newest first, the working-copy parent included: it is a commit to review"
    );
    assert!(r.parent().starts_with(&stack[0].node));
    // A git repository offers no stack rows; a recent commit is typed there.
    assert!(vcs::list_stack(VcsKind::Git, &r.root()).unwrap().is_empty());

    // `sl prev` to amend a lower commit is the review loop; the commits above stay offered.
    r.sl(&["goto", "-q", ".^"]);
    let stack = vcs::list_stack(VcsKind::Sapling, &r.root()).unwrap();
    assert_eq!(
        stack.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        ["third commit", "second commit", "first commit"],
        "the draft descendants of `.` are still commits to review"
    );
}

#[test]
fn the_stack_offers_the_live_successor_of_an_obsolete_working_copy_parent() {
    let r = sl_repo_or_skip!();
    r.write("a.txt", "one\n");
    r.commit_all("first commit");
    r.write("b.txt", "two\n");
    r.commit_all("second commit");
    let stale = r.parent();

    // The amend that leaves the working copy behind. It happens whenever the commit under
    // `.` is rewritten from elsewhere, and `sl` lets the reviewer sit on the dead node.
    r.write("b.txt", "two, amended\n");
    r.sl(&["amend"]);
    let live = r.parent();
    assert_ne!(live, stale);
    r.sl(&["unhide", &stale]);
    r.sl(&["goto", "-q", "--clean", &stale]);
    assert_eq!(r.parent(), stale, "the working copy is parked on the obsolete commit");

    let stack = vcs::list_stack(VcsKind::Sapling, &r.root()).unwrap();
    let nodes = stack.iter().map(|c| c.node.as_str()).collect::<Vec<_>>();
    assert!(
        nodes.iter().any(|n| live.starts_with(n)),
        "the row names the commit that replaced the dead one; got {nodes:?}"
    );
    assert!(
        !nodes.iter().any(|n| stale.starts_with(n)),
        "the dead node is never offered as a commit to review; got {nodes:?}"
    );
}

#[test]
fn a_changed_binary_costs_no_payload_and_leaves_its_neighbour_alone() {
    let r = sl_repo_or_skip!();
    // A blob whose git-mode payload dwarfs the rest of the diff, so a build that reads it
    // is reading a megabyte to learn (0, 0).
    let bin = r.path().join("img.bin");
    std::fs::write(&bin, (0..=255u8).cycle().take(400_000).collect::<Vec<_>>()).unwrap();
    r.write("t.txt", "hello\n");
    r.commit_all("base");
    std::fs::write(&bin, (0..=255u8).rev().cycle().take(400_000).collect::<Vec<_>>()).unwrap();
    r.write("t.txt", "hello\nworld\n");

    let files =
        vcs::changed_files(VcsKind::Sapling, &r.root(), Scope::Uncommitted, None, None).unwrap();
    let by_path = |p: &str| files.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("{p}"));
    // The `--no-binary` spelling itself is under test: `sl` aborts on an unknown flag, and
    // the build fails whole, so a rejected flag never reaches these assertions.
    assert_eq!(by_path("img.bin").kind, ChangeKind::Modified);
    assert_eq!((by_path("img.bin").additions, by_path("img.bin").deletions), (0, 0));
    assert_eq!((by_path("t.txt").additions, by_path("t.txt").deletions), (1, 0));
}

#[test]
fn the_base_picker_leads_with_whole_stack_and_picks_a_commit_by_description() {
    let r = sl_repo_or_skip!();
    stacked_repo(&r);
    let root = r.root();
    let _guard = StoreGuard::for_root(&root);
    let mut app = herdr_reviewr::app::App::new(root.clone(), Scope::Branch, None);
    app.reload().unwrap();
    app.open_base_picker();

    let bp = app.base_picker.as_ref().expect("the picker opens");
    assert!(bp.rows[0].is_default(), "the whole-stack row leads, and picking it clears the pick");
    let titles: Vec<&str> = bp
        .rows
        .iter()
        .filter_map(|row| match row {
            herdr_reviewr::app::BaseChoice::Commit { title, .. } => Some(title.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(titles, ["third commit", "second commit", "first commit"]);

    // The filter matches the description, not only the hash.
    let bp = app.base_picker.as_mut().unwrap();
    bp.query = "second".into();
    bp.caret = bp.query.chars().count();
    bp.cursor = 0;
    let picked = match app.base_picker.as_ref().unwrap().visible().as_slice() {
        [one] => (*one).clone(),
        other => panic!("one row matches `second`, got {}", other.len()),
    };
    let node = picked.name().to_string();
    assert_eq!(picked.pick_spelling(), format!("{node}^..{node}"), "the pick records both ends");
    app.base_picker_pick().unwrap();
    assert!(
        app.branch_tip.as_ref().is_some_and(|t| t.oid.starts_with(&node)),
        "picking a commit row pins the range's far end to it"
    );
    assert_eq!(
        app.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        ["second.txt"],
        "the view is that commit alone"
    );

    // The whole-stack row is the way back: it clears the pick.
    app.open_base_picker();
    let bp = app.base_picker.as_mut().unwrap();
    assert!(
        matches!(bp.rows[bp.cursor], herdr_reviewr::app::BaseChoice::Commit { .. }),
        "the picker reopens on the commit under review"
    );
    bp.cursor = 0;
    app.base_picker_pick().unwrap();
    assert_eq!(app.branch_tip, None, "no pinned far end: the range ends at the working copy");
    assert_eq!(
        herdr_reviewr::sl::Store::open(&root).read_base_pick().unwrap(),
        None,
        "the pick is cleared, so the chain falls back to the public base"
    );
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

#[test]
fn a_sapling_pane_offers_the_changes_tab_alone() {
    let r = sl_repo_or_skip!();
    stacked_repo(&r);
    let root = r.root();
    let _guard = StoreGuard::for_root(&root);
    let mut app = herdr_reviewr::app::App::new(root, Scope::Uncommitted, None);
    app.reload().unwrap();
    app.keys_expanded = true; // the `go` band, where the tab digits would sit

    let out = painted(&app);
    let bar = out.lines().next().unwrap();
    assert!(bar.starts_with(" Changes "), "the one tab reads as a heading, not a control: {bar:?}");
    for absent in ["1 Changes", "Files", "PR"] {
        assert!(!bar.contains(absent), "{absent:?} has no place in a Sapling tab bar: {bar:?}");
    }
    assert!(!out.contains(" tabs"), "the footer offers no tab digits either:\n{out}");

    // The keys are inert, so a digit typed out of habit cannot land on a hidden tab.
    for tab in [herdr_reviewr::app::Tab::AllFiles, herdr_reviewr::app::Tab::Pr] {
        app.set_tab(tab).unwrap();
        assert_eq!(app.tab, herdr_reviewr::app::Tab::Changes, "{tab:?} is not offered");
    }
}

#[test]
fn last_turn_diffs_a_sapling_worktree_against_the_snapshot_baseline() {
    let r = sl_repo_or_skip!();
    r.write("a.txt", "one\n");
    r.commit_all("base");
    let root = r.root();
    let _guard = StoreGuard::for_root(&root);
    let mut app = herdr_reviewr::app::App::new(root.clone(), Scope::LastTurn, None);
    let mut host = herdr_reviewr::world::TurnHost::open(root.clone(), VcsKind::Sapling);
    let mut observe = |app: &mut herdr_reviewr::app::App, status| {
        let sample = herdr_reviewr::herdr::AgentSample {
            cwd: Some(root.to_string_lossy().into_owned()),
            status,
        };
        let report = host.observe_agents(Some(&[sample]));
        let baseline = host.baseline().map(str::to_string);
        app.sync_turn_baseline(baseline.clone());
        app.sync_agents_present(report.agents_present);
        baseline
    };

    app.reload().unwrap();
    assert!(app.awaiting_turn(), "no baseline until a turn is observed");

    observe(&mut app, herdr_reviewr::turn::Status::Idle);
    observe(&mut app, herdr_reviewr::turn::Status::Working); // the candidate pins the turn's start
    // The turn edits a tracked file, adds an untracked one, and commits a third change.
    r.write("a.txt", "one\ntwo\n");
    r.write("new.txt", "fresh\n");
    r.write("committed.txt", "landed mid-turn\n");
    r.commit_all("a commit made during the turn");
    observe(&mut app, herdr_reviewr::turn::Status::Working);
    let baseline = observe(&mut app, herdr_reviewr::turn::Status::Idle);

    app.reload().unwrap();
    assert!(!app.awaiting_turn(), "the baseline is set");
    let mut paths = app.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>();
    paths.sort_unstable();
    assert_eq!(
        paths,
        ["a.txt", "committed.txt", "new.txt"],
        "a commit made during the turn stays in the turn's diff"
    );
    let a = app.entries.iter().find(|e| e.path == "a.txt").unwrap();
    let annotation = a.annotation.as_ref().expect("a changed file is annotated");
    assert_eq!((annotation.change, annotation.additions), (ChangeKind::Modified, 1));
    // The baseline side is the turn's start, not the working-copy parent the commit moved.
    let baseline = baseline.expect("a baseline");
    assert_eq!(vcs::file_content(VcsKind::Sapling, &root, &baseline, "a.txt"), "one\n");
}
