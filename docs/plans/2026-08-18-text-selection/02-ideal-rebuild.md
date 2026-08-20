# Ideal rebuild: Plan

Builds on `01-gesture-and-surfaces.md`. Nothing is externally observed yet: every shape may
change.
Why now: three review rounds patched one seam, the reset showed the frame was wrong, and the
bar for shipping is the cleanest implementation.

Delivers `specs/text-selection.md` (the gesture-end table and `TS-NO-SILENT-LOSS`).

## Problem

Milestone 1 is green but wrong in frame. A drag released past the pane edge silently loses its
copy: the release lands in the next pane and the gesture cancels, recreating issue #62's silent
clipboard failure. The freeze is ~150 lines of stored deferred state (`Deferred` in
`src/app.rs`) duplicating guards the completion channels already enforce. The diff-pane hit
tests re-derive the renderer's layout (`read_slots` mirroring `render_diff_view`), and that
agreement seam produced about a third of all review findings across three rounds.

## Goal

The spec's gesture-end table, implemented with the fewest parts: one gesture state, no stored
deferred results, and hit tests that index a paint-time recording.

## Definition of Done

- [x] A drag released past the pane border copies what was highlighted (`TS-NO-SILENT-LOSS`):
      buttonless motion, the next mouse-down, and the exit deadline each complete a visible
      selection with its `copied N lines` status.
- [x] The exit deadline runs on its own named constant, never `--poll`, and fires only when
      the pointer's last event sat on the border or beyond. A still pointer inside the
      content never completes: a release there would have arrived.
- [x] A press that never moved dissolves on proof or exit deadline, copying nothing. A lost
      gutter drag dissolves without opening the composer.
- [x] A keypress or resize cancels, and the input still acts. A config layout or theme change
      completes the copy.
- [x] A live gesture holds only what it anchors to, with no stored snapshot: a navigator drag
      gates the world drain, a `PR` drag gates the PR drain, and a view-anchored drag lands
      the lists while the open view reloads once at the gesture's end. `Deferred`,
      `apply_pending_world`, and `defer_reveal` are deleted. The completing tick drains its
      gated channel to empty and lands only the last matching generation.
- [x] The gesture is one enum replacing the `text_drag` + `pending_click` + `gutter_drag`
      flag set, so a head without an origin is unrepresentable.
- [x] The diff pane's display-line layout is one walk with two call sites: `render_diff_view`
      records it beside `painted_links` for hover, highlight, and mouse-down hit tests, and a
      post-scroll extent update re-runs it. `read_slots` as a second implementation is
      deleted.
- [x] A `PR`-navigator row copies its full text even when the pane truncates it.
- [x] Milestone 1's behaviors keep passing: all five surfaces, multi-click at release, wheel
      and border extension, source-text copy, the gutter `+` flow.
- [x] `bench_tui.py` medians match the baseline under an interleaved A/B run.
- [x] `just ci` is green.

## Out of Scope

- Everything in `specs/text-selection.md` Non-goals.
- Reworking the preview, PR read pane, and navigator geometry. Those hit tests already derive
  from the same functions their renderers call. Only the diff pane has a mirror.

## Execution Plan

1. [x] Lifecycle in `src/app.rs` and `src/lib.rs`: one gesture enum with end verbs complete
       (copy), dissolve, and cancel; proofs (`Moved`, the next `Down`) complete a moved
       gesture; the exit deadline (own constant, exit signature only) completes it; reflow
       inputs cancel; a config change completes; the gutter arm dissolves on proofs. The
       drag's horizontal edge scroll caps at the widest visible row, so a held border drag
       cannot strand `h_scroll` past all content.
2. [x] Freeze in `src/lib.rs` and `src/app.rs`: per-anchor drain gates with no stored
       snapshot (world gated by a navigator drag, PR gated by a `PR` drag, the open-view
       reload held by one bit for view-anchored drags); drain-to-empty on the completing
       tick; delete `Deferred` and every feeder (`reconcile_world` gesture branches,
       `apply_pr`'s defer, the `clear_pr` and `set_config_error` drops).
3. [x] Recording in `src/ui.rs`: extract the display-line walk `render_diff_view` already
       performs into one function; paint records its result into the painted-frame snapshot
       (the `painted_links` pattern) for hover, highlight, and mouse-down hit tests; the
       wheel and edge-scroll extent updates re-run the walk against post-scroll state;
       delete `read_slots` as a second implementation.
4. [x] Tests: rework the freeze tests around the drain gates, add the overshoot-release,
       exit-deadline, and interior-stillness tests, flip the Moved-cancel test to
       Moved-complete, and add a gutter gesture with the find band open.
5. [x] Bench A/B against a rebuilt baseline on a quiet system, then `just ci`.

## Likely Files

| file                | change                                                   |
| ------------------- | --------------------------------------------------------- |
| `src/app.rs`        | gesture enum, delete `Deferred` and its feeders           |
| `src/lib.rs`        | end verbs and proofs, drain gate, deadline complete       |
| `src/ui.rs`         | paint-time recording, delete `read_slots` mirror helpers  |
| `tests/app_flow.rs` | lifecycle and freeze tests reworked                       |
| `tests/render.rs`   | recording-backed hover and highlight tests                |

## Verification

- `just ci` → green.
- `TS-NO-SILENT-LOSS` → `a_release_lost_past_the_border_still_copies` → a lost release copies
  the highlight.
- `TS-NO-REVIEW-STATE` → `a_release_on_the_mouse_down_cell_is_a_click_and_a_real_drag_copies`
  → unchanged and passing.
- `TS-ONE-SURFACE` → `ts_one_surface_a_drag_clamps_to_its_pane_and_skips_cards` → unchanged
  and passing.
- `python3 scripts/bench_tui.py --fixture` A/B → medians within noise of baseline.
- Tight: everything the diff adds is exercised by a DoD line. Delete or defer the rest.
- Gate: promote `text-selection.md`, `input.md`, `diff-view.md`, `overview.md` to Current.

## Replan

- 2026-08-19: review round 4 died without a final report. Its interim candidates are absorbed:
  the deadline-cancel corroboration became the exit deadline, the unbounded `h_scroll` became
  step 1's cap, the gutter-with-find candidate became step 4's test, and the rest review code
  the rebuild deletes. The merge gate's fresh review loop supersedes the round.
- If herdr grows a forwarded release or pane-leave event, then the exit deadline collapses
  into it (specs/text-selection.md Non-goals).
- 2026-08-19: grok gut check → the deadline decoupled from `--poll` and scoped to the exit
  signature (interior stillness is a held button), the freeze back to per-anchor granularity
  without stored snapshots, the layout walk made one function with two call sites, the
  multi-click copy put into the end table → `specs/text-selection.md`, this plan.
- 2026-08-19: opened from the /thebar reset.
