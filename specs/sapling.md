---
Status: Current
Created: 2026-08-21
Last edited: 2026-09-04
---

# Sapling repositories

Reviewing a worktree whose version control is Sapling (`sl`) instead of git.

## Overview

A pane detects its repository's kind once, at open. Resolution tries the git top level first,
then `sl root`. A root that both git and Sapling claim is reviewed as git. The kind is fixed for
the pane's lifetime, so a directory that becomes a Sapling repository after open needs a reopen.

The review loop is unchanged: the `Changes` tab, the three scopes, comments, and export behave as
`review-model.md` states. This doc owns only the deltas a Sapling repository forces. Every scope
stays O(changed files): a monorepo worktree holds millions of files, so no Sapling read may
enumerate the worktree.

| git concept                | Sapling equivalent                                          |
| -------------------------- | ----------------------------------------------------------- |
| `HEAD`                     | the working-copy parent, `.`                                 |
| the index / staged changes | none; the working copy is the only uncommitted state        |
| `origin/HEAD`              | none; the last public ancestor is the default base          |
| private refs               | none; reviewr's own state directory (the snapshot store)    |

## Invariants

| code                | Always true                                                             |
| ------------------- | ----------------------------------------------------------------------- |
| `SL-NO-REPO-WRITES` | In a Sapling repository reviewr writes nothing inside the repository.   |
| `SL-SCALE-CHANGED`  | Every Sapling read costs O(changed files), never O(worktree files).     |

`SL-NO-REPO-WRITES` is stricter than the git `No writes` invariant (`overview.md`): git gets
private refs, Sapling gets none, because Sapling has no ref store a plugin may write without
entering the repo's own metadata. The turn baseline and the base pick live in the snapshot store.

## The snapshot store

One directory per worktree under reviewr's state directory, keyed by the worktree-path hash the
git baseline ref already uses. It holds the base pick and the turn baselines. Deleting it loses
the pick and the baseline, never a comment or repository state.

A turn baseline is a manifest, not a tree object: the working-copy parent commit plus the dirty
files at snapshot time, each dirty file's content stored as a content-addressed blob. A file
clean at snapshot time is not stored; its baseline content reads from the parent commit at diff
time. The baseline's identity is a digest over the parent and the dirty entries, so the
divergence check that promotes a candidate compares digests exactly as git compares tree ids.

- The `last-turn` changed set is every path whose current content differs from its baseline
  content. Candidates come from the baseline's dirty set and `sl status --rev <parent>`, so
  commits made during the turn stay in the diff.
- A turn-start-dirty file's counts are computed in-process from the stored bytes and the
  worktree, because its baseline side exists only in the store. Every other candidate had the
  parent's content at the turn start, so its kind and counts come from that status pass and one
  `sl diff` — never one subprocess per file.
- A turn-start-dirty file past the diff pane's render budget counts `(0, 0)`, as a binary one
  does.
- The current baseline persists in the store and survives pane restarts. A baseline the
  store no longer holds whole reads at open as no baseline, never as an error.
- Writing a new baseline keeps a small tail of recent baselines and drops the rest. Two
  panes share one worktree's store, so a promotion in one must not delete the other's
  live baseline.
- A blob drops only when every kept manifest was read and none of them names it. One
  manifest that fails to read leaves the whole sweep for the next promotion.
- A blob written in the last minute stays whatever the manifests name. A sibling pane
  writes its blobs before the manifest that names them, so the sweep cannot yet tell that
  blob from an abandoned one.

## Scopes

| scope         | shows                                                                     |
| ------------- | ------------------------------------------------------------------------- |
| `uncommitted` | working copy vs its parent commit, plus untracked files                   |
| `branch`      | working copy vs the merge base (`ancestor`) of the base and `.`           |
| `last-turn`   | the snapshot-store baseline vs the working copy                           |

The base chain keeps its order and its skip-never-error contract (`review-model.md` Base
branch); only the sources adapt:

| # | source              | base is                                                    |
| - | ------------------- | ---------------------------------------------------------- |
| 1 | `--base <rev>` flag | the revision, resolved verbatim                            |
| 2 | the worktree pick   | the revision named in the base picker                      |
| 3 | the public base     | `last(public() & ::.)`, when the repo has public commits   |

- The pick is one revision spelling per worktree, stored in the snapshot store. Sapling
  worktrees share no store reviewr may write, so the pick is per-worktree, not per-repository.
- An empty `public()` answer is a no-base state, never an error.
- A `branch` range has two ends. It starts at the base and ends at the working copy, unless the
  pick names one commit to review, which ends it at that commit.
- The header names the far end only when the pick pins it, `vs f01c3d2a4b56 → 2eb84b9c0d1e`.
  A range that ends at the working copy names the base alone, as a git repository does
  (`tui.md`).
- The pinned far end names its code review number, `vs f01c3d2a4b56 → D113340447`, and its
  abbreviated node when it carries none, exactly as its base-picker row does.
- The pinned far end's description follows the nodes,
  `vs f01c3d2a4b56 → 2eb84b9c0d1e · fix the thing`, and a header too narrow for all of it
  drops the description before the nodes.
- The base picker lists `whole stack` first, then the local bookmarks, then the draft commits
  connected to `.`, newest first. Connected runs both ways, so a `sl prev` down the stack still
  offers the commits above. A typed spelling resolves through `sl log -r`.
- The public base gets no row of its own. It is what `whole stack` selects, and a second row
  naming it would pin the node it resolves to today.
- A stack row resolves through its successors, so a working copy parked on an obsolete commit
  offers the node that replaced it. A successor that landed is public, and its row goes with it.
- Picking a commit row reviews that commit alone, against its own parent. No merge base stands
  between the two: the range's ends are the commit and its parent, whatever `.` has become.
- A picked commit resolves through its successors, so an `amend` or a `rebase` moves the review
  onto the commit's new node, never leaves it on the node that was replaced.
- The parent re-derives from the resolved commit, never from the recorded spelling, so a rebased
  commit diffs against the parent it has now.
- A commit pick names its commit in the export's preamble, the one range whose reviewed state
  is not the worktree the agent edits (`review-model.md` Export).
- Picking `whole stack` clears the pick, so the chain falls back to the public base.
- A commit pick records both ends, `<node>^..<node>`, so it still names one commit after a
  restart. The pick skips whole when its commit has no live successor, and when that
  successor is a root commit with no parent to diff against.
- A dormant pick names a node abbreviated, `2eb84b9c0d1e missing`, and a bookmark name whole.
- A commit row reads as its description's first line. It is marked with its code review
  number, and with the short hash it records when it carries no number. The filter matches
  the description, the hash, and the number.
- A typed code review number matches the stack rows, never a live revision probe. Sapling
  pulls an unknown bare symbol from the remote, and `SL-NO-REPO-WRITES` forbids the write.
- Every probed spelling resolves inside `present()`, so an unknown one is empty rather than
  pulled. A name that looks like a remote one is otherwise fetched before Sapling gives up
  on it, and the picker probes per keystroke while a dormant pick probes per poll.
- The picker names commits, not branches: its title is `Pick base commit` and a filter matching
  no row says `no commits match`.
- A digit-only spelling resolves as a hash prefix, never as a local revision number. Bare in
  a revset, `123456` names a decade-old commit.
- Every other spelling resolves as Sapling resolves it bare, so a bookmark named `beef`
  resolves as that bookmark rather than as a hash prefix.
- Every node the pane names paints at twelve hex, Sapling's short-node width, never git's
  seven.
- A typed hash prefix records the whole node it resolved to. A typed name records itself
  and keeps following its bookmark.
- A spelling whose prefix has gone ambiguous is skipped, exactly as an unknown one.
- There is no default-branch name, so the pick can only be replaced, never cleared, exactly as
  a git repository with no default branch.

## Reads

Every Sapling command runs with `HGPLAIN=1`, with the repository root as its working directory,
and parses stdout only. The enumerator is `sl status -Tjson -C`: JSON rather than the indented
copy-source lines, `--copies` so a rename diffs real content. The parent pin is `sl whereami`,
first line, because it answers in milliseconds where `sl log` pays command dispatch. Content at
a revision is `sl cat -r`; exit 1 is absence, read as empty content. Counts for `uncommitted`
and `branch` parse one `sl diff --git --no-binary` per build. A binary file counts `(0, 0)`
either way, so the diff names it and never carries its payload. A range pinned at both ends reads once and
caches: two commits' changed set cannot change, so a later poll costs only the pick's own
resolution. The base picker's commit rows are one
`sl log -r` over the draft commits connected to `.`, never over the repository's whole draft set.
That read and the bookmark read run together, so opening the picker costs one wait, not two.

| status code | changed-file kind                                    |
| ----------- | ---------------------------------------------------- |
| `M`         | modified                                             |
| `A`         | renamed when its copy source is also `R`, else added |
| `R`, `!`    | deleted                                              |
| `?`         | untracked                                            |

A copy leaves its source in place, so it reviews as a whole new file: its counts are its own line
count, not the delta from the source that `sl diff` measures.

Agent membership (`herdr-host.md`) resolves an agent's directory through `sl root` in a Sapling
pane: a resolved root is cached, a non-root answer is re-checked, a spawn failure holds the
sample.

## Disabled surfaces

Each of these would enumerate or index the worktree, which `SL-SCALE-CHANGED` forbids.

| surface     | in a Sapling pane                                                      |
| ----------- | ----------------------------------------------------------------------- |
| `All files` | not offered; the tab never paints and its key is inert                 |
| `PR` tab    | not offered; no probe or fetch ever spawns                             |
| search      | does not open; the status line says search needs a git repository      |

`Changes` is then the only tab, with nothing to switch to, so it reads as a heading without its
key and the footer drops the tab keys (`input.md`).

## Failure semantics

- A failed Sapling command fails the build whole; the stale frame stays and the status reports,
  matching the git contract (`overview.md` Continuity).
- The status names the failed command as it would be typed, `sl status -mardu -C -Tjson: abort: …`.
- A crash between manifest write and baseline update costs at most that turn's baseline; the
  previous baseline stays live.
- A merge in progress pins to the first parent.

## Non-goals

- No Phabricator tab. The `PR` tab stays a forge mirror for git repositories.
- No commit-cloud or `sl snapshot` integration. The baseline never leaves the machine.
- No ignored-file enumeration, in any tab.
- No mid-session kind switch. Reopen the pane.

## Related specs

- [overview](./overview.md)
- [review-model](./review-model.md)
- [input](./input.md)
- [herdr-host](./herdr-host.md)
- [file-list](./file-list.md)
- [search](./search.md)
