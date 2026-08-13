# PR #55 takeover — IME caret anchoring: Plan

Delivers `specs/input.md` (input caret), `specs/search.md`, `specs/find-in-file.md` (PR #55).

## Problem

PR #55 (`tomotochi:fix/ime-cjk-caret`) fixes CJK IME anchoring but cannot merge. It conflicts with
main in `CHANGELOG.md`. Its `row_with_caret` change silently removed the base picker's end-of-input
caret, so the picker shows no caret at all while typing at the end of the filter. The composer's
terminal cursor can land on the box border when a wrapped row is exactly full. Search's empty-query
branch calls `row_with_caret("", 0, p)`, which now renders nothing.

## Goal

The PR merges clean, every text input (composer, search, find, base picker) anchors the IME at the
caret in display cells, and no input loses its visible caret.

## Definition of Done

- [ ] `gh pr view 55` reports `mergeable: MERGEABLE`.
- [ ] The composer's terminal cursor never leaves the box interior (test: exactly-full wrapped row).
- [ ] The base picker shows the terminal cursor at the caret in display cells, including at end of
      input and on an empty query (test).
- [ ] The search band's empty-query render is unchanged with the dead call removed.
- [ ] `just ci` passes.
- [ ] The branch is pushed to `tomotochi/fix/ime-cjk-caret` with a PR comment naming the changes.

## Out of Scope

- Dropping the painted block caret in favor of the terminal cursor everywhere. Noted on the PR as a
  design observation only.
- A fresh A/B latency bench. The PR's bench stands. This delta adds one `set_cursor_position` and an
  O(query-length) scan in overlay inputs only. Veto this exclusion to get a full interleaved run.

## Execution Plan

1. [ ] Merge `origin/main` into the branch. Resolve `CHANGELOG.md`: keep the v0.30.2 release block,
       keep the PR's Fixed entry under Unreleased.
2. [ ] Shared input view (src/ui.rs): one helper over `single_line_caret_view` that returns the
       input's spans (placeholder on empty) and the cursor's cell column. Port the search band, find
       band, and base picker onto it, deleting their bespoke empty/scroll branches. Each call site
       sets the terminal cursor from the helper's column.
3. [ ] Composer clamp: bound `composer_caret_cell_position`'s column to the content width inside the
       function. Unit test beside `comment_caret_follows_the_existing_wrap_rows`.
4. [ ] Specs: state the terminal-cursor contract once in `specs/input.md`'s field section, covering
       every text field. Delete the duplicated sentences from `specs/search.md` and
       `specs/find-in-file.md`. Extend the CHANGELOG entry with the base picker.
5. [ ] Tests: base picker caret render test, full-row clamp unit test, existing render tests green.
6. [ ] `just ci`.
7. [ ] Merge gate: high-effort `/code-review` on the full branch diff until clean, then `/garfield`.
       Verify the four touched specs against the code and promote to Current.
8. [ ] Push to `tomotochi/fix/ime-cjk-caret`. Comment on PR #55: takeover note, the base picker
       regression fix, the unification, the clamp.

## Likely Files

| file                    | change                                                    |
| ----------------------- | --------------------------------------------------------- |
| `CHANGELOG.md`          | conflict resolution, base picker line in the Fixed entry  |
| `src/ui.rs`             | composer clamp, base picker caret, search cleanup, tests  |
| `specs/input.md`        | Base picker cursor sentence                               |
| `tests/render.rs`       | base picker caret test                                    |

## Verification

- `cargo test` unit + `cargo test --test render` → new tests green.
- `just ci` → green.
- Tight: every diff addition is exercised by a DoD line.
- Gate: promote `input.md`, `search.md`, `find-in-file.md` to Current if the PR left them Draft.

## Replan

- If the base picker fix reveals more `row_with_caret` end-of-input callers, sweep them in the same
  pass and log here.
- 2026-08-13: weighed reserving a wrap cell against the continuation row → user chose the
  continuation row: the growing empty row shows where the next character lands.
- 2026-08-13: three review rounds each found a bug in the composer caret seam → bar reset →
  `box_rows` keeps a continuation row after an exactly-full row, so every legal caret has a row
  and the downstream clamp, phantom row, and sizing patches are deleted.
- 2026-08-13: user chose the from-scratch shape → shared input view for all single-line inputs,
  cursor contract stated once in `input.md` → steps 2 and 4 rewritten.
- 2026-08-13: initial plan. Base picker regression found during grounding → added as step 3.
