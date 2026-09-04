---
Status: Current
Created: 2026-08-21
Last edited: 2026-09-03
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
- The current baseline persists in the store and survives pane restarts.
- Writing a new baseline keeps a small tail of recent baselines and drops the rest. Two
  panes share one worktree's store, so a promotion in one must not delete the other's
  live baseline.

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
- The header names the far end only when the pick pins it, `vs f01c3d2 → 2eb84b9`. A range that
  ends at the working copy names the base alone, as a git repository does (`tui.md`).
- The base picker lists `whole stack` first, then the local bookmarks, then `.` and its draft
  ancestors, newest first. A typed spelling resolves through `sl log -r`.
- Picking a commit row reviews that commit alone, against its own parent. No merge base stands
  between the two: the range's ends are the commit and its parent, whatever `.` has become.
- A commit pick names its commit in the export's preamble, the one range whose reviewed state
  is not the worktree the agent edits (`review-model.md` Export).
- Picking `whole stack` clears the pick, so the chain falls back to the public base.
- A commit pick records both ends, `<node>^..<node>`, so it still names one commit after a
  restart. An end that has gone away skips the whole pick.
- A commit row reads as its description's first line and is marked with the short hash it
  records. The filter matches the description and the hash.
- The picker names commits, not branches: its title is `Pick base commit` and a filter matching
  no row says `no commits match`.
- A hex-shaped spelling resolves as a hash prefix, never as a local revision number —
  bare in a revset, `123456` names a decade-old commit.
- A spelling whose prefix has gone ambiguous is skipped, exactly as an unknown one.
- There is no default-branch name, so the pick can only be replaced, never cleared, exactly as
  a git repository with no default branch.

## Reads

Every Sapling command runs with `HGPLAIN=1`, with the repository root as its working directory,
and parses stdout only. The enumerator is `sl status -Tjson -C`: JSON rather than the indented
copy-source lines, `--copies` so a rename diffs real content. The parent pin is `sl whereami`,
first line, because it answers in milliseconds where `sl log` pays command dispatch. Content at
a revision is `sl cat -r`; exit 1 is absence, read as empty content. Counts for `uncommitted`
and `branch` parse one `sl diff --git` per build. The base picker's commit rows are one
`sl log -r` over the draft ancestors of `.`, never over the repository's whole draft set.

| status code   | changed-file kind                          |
| ------------- | ------------------------------------------ |
| `M`           | modified                                   |
| `A`           | added; with a copy source, renamed         |
| `R`, `!`      | deleted                                    |
| `?`           | untracked                                  |

Agent membership (`herdr-host.md`) resolves an agent's directory through `sl root` in a Sapling
pane: a resolved root is cached, a non-root answer is re-checked, a spawn failure holds the
sample.

## Disabled surfaces

Each of these would enumerate or index the worktree, which `SL-SCALE-CHANGED` forbids. Each
paints a calm one-line state, never an error.

| surface     | in a Sapling pane                                                      |
| ----------- | ----------------------------------------------------------------------- |
| `All files` | lists nothing and says the tab needs a git repository                  |
| search      | does not open; the status line says search needs a git repository      |
| `PR` tab    | a static state naming git; no probe or fetch ever spawns               |

## Failure semantics

- A failed Sapling command fails the build whole; the stale frame stays and the status reports,
  matching the git contract (`overview.md` Continuity).
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
