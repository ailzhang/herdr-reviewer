# Open on the live foreground cwd: Plan

Delivers `specs/herdr-host.md#repo-discovery` by finishing PR #59 (external contribution, branch `fix/open-cwd-live-foreground`).

## Problem

Opening reviewr beside a pane running `claude -w <worktree>` reviews the main checkout's branch, not the worktree's. herdr's action context carries the pane's launch cwd, and `claude -w` chdirs into the worktree only inside its own process. PR #59 fixes the case but diverges from the locked design in two ways: a live cwd outside any git repo refuses an open the launch cwd could place, and the live read runs on every mode including close.

## Goal

PR #59 merged, matching the Repo discovery contract exactly: live foreground cwd when it lies in a git repo, launch cwd otherwise, refusal only when neither qualifies, and no live read off the open path.

## Definition of Done

- [x] An open beside a `claude -w <worktree>` pane reviews the worktree (PR's existing test `open_prefers_the_focused_panes_live_foreground_cwd`).
- [x] A live cwd outside any git repo falls back to the launch cwd instead of refusing (`a_toggle_open_falls_back_when_the_live_cwd_is_not_a_repo`, run as a toggle).
- [x] A missing live cwd falls back to the launch cwd (PR's existing test `open_keeps_the_context_cwd_without_a_live_foreground_cwd`).
- [x] No action pays an extra herdr call: the live cwd comes from the pane-list snapshot the run already holds.
- [x] The event open takes the payload cwd even when the live cwd is a repo (`auto_open_takes_the_event_payload_cwd_over_the_live_one`).
- [ ] PR #59 merges with the contributor's commits intact.

## Out of Scope

- Refusal message wording. Builder's mechanism, spec owns only the refusal itself.
- The `docs/herdr-api-notes.md` addition. Already correct in the PR, lands as-is.

## Execution Plan

1. [x] `gh pr checkout 59`, then rebase onto `main` if behind.
2. [x] In `herdr/pane.sh`: move the live-cwd block onto the open path, just above the repo check, gated `[ "$mode" != auto-open ]`.
3. [x] In the same block: read `foreground_cwd` from the held `panes_json` snapshot and accept it only when `is_git_repo` passes, else keep the context cwd.
4. [x] In `tests/pane_actions.rs`: serve `foreground_cwd` through `pane_with_cwd` pane-list fixtures, drop the fake's `pane get` verb, add the non-repo-fallback toggle test and the event-payload-wins auto-open test.
5. [x] Commit on top of the contributor's commits and push to their branch (`maintainerCanModify` is true).

## Likely Files

| file                     | change                                                        |
| ------------------------ | ------------------------------------------------------------- |
| `herdr/pane.sh`          | relocate the live read to the open path, add the repo guard   |
| `tests/pane_actions.rs`  | one fallback test, two no-live-read assertions                |
| `specs/herdr-host.md`    | promote Repo discovery Draft to Current at the merge gate     |

## Verification

- `cargo test --test pane_actions` → all pass, including the PR's two existing tests unchanged.
- `just ci` → clean.
- Tight: everything the diff adds is exercised by a DoD line. Delete or defer the rest.
- Gate: high-effort `/code-review` loop on the branch until clean (4 rounds), then `/garfield` end to end (pass), then promote `specs/herdr-host.md` to Current. Done.

## Replan

- If pushing to the fork branch fails despite `maintainerCanModify`, then recreate the branch in this repo from their head with commits intact and retarget the merge.
- 2026-08-12: review found `pane list` entries already carry `foreground_cwd` (docs/herdr-api-notes.md, verified live 0.7.5) → the live read reuses the run's held snapshot and `pane get` drops entirely → pane.sh, tests, DoD.
- 2026-08-12: initial plan.
