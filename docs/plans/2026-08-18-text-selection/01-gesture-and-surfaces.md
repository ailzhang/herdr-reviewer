# Text selection: Plan

Delivers `specs/text-selection.md`, the gutter comment gesture in `specs/input.md`, and the gutter
`+` in `specs/diff-view.md` (issue #62, supersedes PR #65).

## Problem

A reviewer in a herdr pane cannot copy text they see in reviewr — a diff line, a filename, PR
comment text — to paste into the agent session. Mouse capture disables native terminal selection,
and shift+drag selects across the whole terminal window including pane chrome (issue #62). PR #65
adds a modal `C` copy mode, which works but hides copy behind a mode nobody discovers.

## Goal

Drag over text selects it character-precise and copies its source text on release. Mouse
commenting moves to the gutter. No mode, no new key to learn.

## Definition of Done

- [x] A body drag in the Diff view highlights characters and, on release, puts the spanned source
      text on the clipboard with a `copied N lines` footer status.
- [x] A wrapped line copies as its one source line. Gutter cells, folds, notices, and spliced
      comment cards contribute nothing.
- [x] The File view, the markdown preview, the `PR` read pane, and both navigators are selectable.
      A file-navigator row copies its repo-relative path.
- [x] Double-click copies the token under the pointer, triple-click the logical line, and in a
      navigator a double-click copies the row while a same-row release still activates it.
- [x] Hovering a commentable row shows `+` in its change-bar cell. A gutter click opens the
      composer on that line. A gutter drag selects the range and opens the composer on release.
      PR snippet rows show no `+`. The gutter is inert while composing.
- [x] A body drag never line-selects (`v` unchanged), and creates no comment or line selection.
- [x] A keypress, resize, or layout change cancels an active drag. A lost release cancels on the
      next keypress or mouse-down, or at the next poll deadline. An active drag freezes the view.
- [x] The wheel and the pane edges extend an active drag. With wrap off, the left and right edges
      scroll horizontally.
- [x] `bench_tui.py` medians match the pre-change baseline under an interleaved A/B run.
- [x] `just ci` is green.

## Out of Scope

- OSC 52 / clipboard over SSH. `specs/herdr-host.md` Non-goals.
- Selection in the overlays (search, find band, comments list, pickers). `specs/text-selection.md`
  Non-goals.
- A copy-on-select toggle or lingering selection. `specs/text-selection.md` Non-goals.
- Merging PR #65. Superseded by this design; its author gets a note with the reshape.

## Execution Plan

1. [x] Endpoint mapping in `src/ui.rs`: extend the `hit_diff` path (ui.rs:254) to return a
       character offset within the hit row, replaying the wrap math for that one row. Wide
       characters and tabs resolve to their whole character. Unit tests beside it.
2. [x] Selection gesture in `src/lib.rs`: a transient anchor/extent struct in the event loop, kind
       and surface locked at mouse-down. Replace the body-drag arm that calls
       `App::drag_select_to` (app.rs:2524) — body drags become text selection, the gutter arm
       routes into the existing line-selection path.
3. [x] Extraction in a new `src/selection.rs`: endpoints plus `Row::text()` (diff.rs:103) →
       clipboard string. Folds, notices, and cards contribute nothing. Copy via
       `export::Clipboard`, status `copied N lines`. Unit tests for endpoint slicing and skips.
4. [x] Highlight render in `src/ui.rs`: style the cells between anchor and pointer over the
       painted frame (PR #65's `render_copy_selection` pattern). Freeze: an active drag joins
       `Mode::is_modal()`'s reconcile gate (app.rs:192) the way composing does.
5. [x] Cancels and scroll: keypress/resize/layout-change cancel, lost-release rules, wheel and
       edge extension, horizontal edges with wrap off.
6. [x] Surfaces beyond the diff: File view rows (same `FileDiff` path), file navigator via
       `hit_file` (ui.rs:215) with repo-relative paths, `PR` read pane and navigator, markdown
       preview — each supplies row → text.
7. [x] Multi-click: click timestamps in the event loop, token boundaries, navigator row
       double-click, chain reset on content change.
8. [x] Gutter commenting: hover `+` in the change-bar cell (track the pointer's last cell —
       unfilter `Moved` at lib.rs:1749 — and repaint only when the hovered row changes), click
       opens the composer as `c`, drag selects then opens on release, inert while composing, no
       `+` on PR snippets or continuation-only cells.
9. [x] Tests: `tests/render.rs` for the highlight and the `+`; `tests/app_flow.rs` for the
       gestures and both invariants (below).
10. [x] Bench: interleaved A/B against a rebuilt baseline binary on a quiet system; attach medians.

## Likely Files

| file                | change                                                       |
| ------------------- | ------------------------------------------------------------- |
| `src/lib.rs`        | gesture routing, multi-click, hover tracking, cancel rules    |
| `src/ui.rs`         | endpoint mapping, highlight render, gutter `+`                |
| `src/selection.rs`  | new: extraction to clipboard text                             |
| `src/app.rs`        | drag-freeze gate, gutter path into line selection             |
| `tests/render.rs`   | highlight and `+` render tests                                |
| `tests/app_flow.rs` | gesture flows, invariant tests                                |

## Verification

- `just ci` → green.
- `TS-NO-REVIEW-STATE` → `a_release_on_the_mouse_down_cell_is_a_click_and_a_real_drag_copies`
  (app_flow) → a full drag-release cycle leaves `CommentStore` and the line selection untouched.
- `TS-ONE-SURFACE` → `ts_one_surface_a_drag_clamps_to_its_pane_and_skips_cards` (app_flow) → a
  drag exiting the pane clamps, a code drag across a card copies no card text.
- `python3 scripts/bench_tui.py --binary target/release/herdr-reviewr --fixture` A/B → medians
  within noise of baseline.
- Tight: everything the diff adds is exercised by a DoD line. Delete or defer the rest.
- Gate: promote `text-selection.md`, `input.md`, `diff-view.md`, `overview.md`, `README.md`
  ownership row to Current.

## Replan

- If herdr does not forward `Moved` events or drops the release in ways the poll-deadline rule
  can't absorb, then revisit the hover `+` and lost-release contracts in the spec.
- If the `Moved`-driven repaint regresses `bench_tui.py` medians, then gate the hover repaint
  harder (row-change only is the floor) before touching the spec.
- If the `PR` read pane's row → text accessor turns out to need per-block plumbing, then step 6
  splits: ship diff/file/navigators first, PR pane and preview in the same branch after.
- 2026-08-18: review round 1 → the poll-deadline cancel spared live drags (mouse-activity
  timer), the PR pipeline gained the gesture freeze (`pending_pr`), the reveal defers with the
  snapshot, PrNav got row slop, the multi-click chain keys on the mapped row, and the splice
  math collapsed into `composing_split` shared by painter and slot map → `src/lib.rs`,
  `src/app.rs`, `src/ui.rs`.
- 2026-08-19: review round 2 → three cancel/scroll contracts corrected in the spec: a lost
  release is now proven by buttonless pointer motion (the poll-deadline cancel killed a
  motionless held drag), edge auto-scroll moved off the outermost content rows onto the
  border, and the multi-click copy moved to the release so a fast press-drag stays a drag.
  Also fixed: a `clamp` panic while composing, stale deferred snapshots re-checked by input
  tag, config-error now drops all deferred state, strict hit tests refuse blank space, a
  wide-char highlight overpaint, and the copied-line count → `specs/text-selection.md`,
  `src/lib.rs`, `src/app.rs`, `src/ui.rs`, `src/selection.rs`.
- 2026-08-19: review round 3 → the gesture freeze re-derived instead of a fourth patch: a
  drag now freezes only the surface it anchors to (like composing), so a lost release can no
  longer freeze every refresh, and all deferred state consolidated into one `Deferred`
  struct with one drop point. The poll deadline returned for never-moved gestures only, a
  theme change cancels like a layout change, painted copies drop pad and rule chrome, the
  edge scroll measures content rects, a whitespace double-click falls back to the click, the
  multi-click copy routes through the one extractor, and the selection hit tests stopped
  allocating per visible row → `specs/text-selection.md`, `src/app.rs`, `src/lib.rs`,
  `src/ui.rs`.
- 2026-08-18: initial plan.
