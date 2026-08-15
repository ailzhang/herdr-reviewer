# PR finding range: Plan

Delivers `specs/pr-tab.md` (navigator and read pane), `specs/forge-host.md` (comments), `specs/forge-providers.md` (range mapping), `specs/diff-view.md` (PR snippet rows).

## Problem

A finding paints the stored `diffHunk` as raw `+`/`−` text. The thread range is one end line in `anchor`. A comment on `manager.py:114-115` reads as `manager.py:115` above a pager dump.

## Goal

The read pane shows the comment line range as Diff-view rows. The navigator shows `path:start-end` when the ends differ.

## Definition of Done

- [x] A GitHub finding with a `diffHunk` and a placeable range shows that range as Diff-view rows, then the markdown body.
- [x] Those rows have syntax highlight, add/delete tints, line numbers, wrap, and word emphasis (`specs/diff-view.md`).
- [x] The painted window is the range plus three stored-hunk lines above and below. Lines farther than that do not show. `@@` headers and `\ No newline at end of file` do not show.
- [x] A hunk with no `@@` still shows tints and highlight, with no line numbers. A line that is not a diff line still shows.
- [x] A one-line comment is a range of one. A finding with no snippet, or a range the hunk cannot place, shows only the body.
- [x] A gutter number in the comment range uses the comment color. A margin line does not. The rows are not cursor, selection, or comment targets.
- [x] A GitHub thread maps `startLine`..`line`, or `originalStartLine`..`originalLine` when the new-side lines are absent. `anchor` is `path:start-end` when the ends differ.
- [x] A GitLab `line_range` and an Azure `rightFileStart`..`rightFileEnd` fill the same `anchor` form. Those findings still have no snippet.
- [x] `pr_bodies_render_as_markdown_and_the_description_row_pins_first` and `an_anchor_in_a_comment_body_jumps_past_the_snippet_offset` still pass against the new rows.

## Out of Scope

- Reconstruct a hunk from the worktree or from an extra forge call. `specs/pr-tab.md` Non-goals.
- Comment-body chrome, suggestion blocks, reply expansion. Same.
- Jump from a finding to the code tabs. Same.
- The wrap key on the PR tab. `src/lib.rs` already drops `Wrap` in the PR arm.

## Execution Plan

1. [x] `diff::rows_from_snippet(hunk, path, start, end, hl)` in `src/diff.rs`. Parse unified-diff lines. Number from `@@`. Keep rows whose number falls in the range. Call `compute_emphasis`. Highlight through `language_of`. Empty `Vec` when the range cannot be placed. Unit tests beside `compute_emphasis`.
2. [x] Shared `finding_anchor(path, start, end)` in `src/forge.rs`. GitHub `merge_comments` reads `startLine`/`line` and the original pair. `build_detail_query` asks for those four fields. Tests next to `a_bots_findings_are_each_kept_even_as_its_prose_collapses`.
3. [x] GitLab `merge_comments` uses `position.line_range`, else `new_line` or `old_line`. Extend `discussions_map_to_findings_comments_and_approvals_to_reviews`. Azure `merge_comments` uses `rightFileStart`..`rightFileEnd`, else the left pair. Change `threads_map_to_comments_findings_and_votes` from `src/main.rs:14` to `src/main.rs:12-14`.
4. [x] `render_pr_read` in `src/ui.rs` paints those rows with `render_row`. `wrap` is true. `commented` is true. `cursor` and `selected` are false. `find` is `None`. Expose `App` highlighter to that call. Update the two `tests/render.rs` tests named in the DoD.

## Likely Files

| file                  | change                                              |
| --------------------- | --------------------------------------------------- |
| `src/forge.rs`        | query fields, range → `anchor`, tests               |
| `src/gitlab.rs`       | `line_range` → `anchor`                             |
| `src/azure_devops.rs` | start..end → `anchor`                               |
| `src/diff.rs`         | `rows_from_snippet`, tests                          |
| `src/ui.rs`           | `render_pr_read` uses `render_row`                  |
| `src/app.rs`          | highlighter access for the PR pane                  |
| `tests/render.rs`     | finding rows, heading-jump offset                   |

## Verification

- `cargo test rows_from_snippet` and `cargo test --test render pr_bodies` → DoD tests pass.
- `just ci` → clean.
- `python3 scripts/bench_tui.py --binary target/release/herdr-reviewr --fixture` A/B against a rebuilt baseline on a quiet system → medians unchanged (render and highlight paths).
- Tight: everything the diff adds is exercised by a DoD line.
- Gate: promote `specs/pr-tab.md`, `specs/forge-host.md`, `specs/forge-providers.md`, and `specs/diff-view.md` to Current.

## Replan

- If a live GitHub `startLine`..`line` pair does not sit on the `diffHunk` `@@` numbers, then the place rule in `rows_from_snippet` needs a side-aware match. Record the rule in `specs/pr-tab.md`.
- 2026-08-15: show three stored-hunk lines above and below the comment range. Peach stays on the range. Infer side from the comment, not the margin.
- 2026-08-15: old-side context still painted `new_no` → copy `old_no` onto kept context before `render_row`.
- 2026-08-15: review found following-context leak on insert-only ranges → side-aware match in `rows_from_snippet` → `specs/pr-tab.md` and `src/diff.rs`.
- 2026-08-15: initial plan.
