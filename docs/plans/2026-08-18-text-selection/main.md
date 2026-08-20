# Text selection: Delivery Strategy

Delivers `specs/text-selection.md`.

## Problem

A reviewer in a herdr pane cannot copy text they see in reviewr — a diff line, a filename, PR
comment text — to paste into the agent session. Mouse capture disables native terminal selection,
and shift+drag selects across the whole terminal window including pane chrome (issue #62). PR #65
adds a modal `C` copy mode, which works but hides copy behind a mode nobody discovers.

## Goal

Drag over text selects it character-precise and copies its source text on release. Mouse
commenting moves to the gutter. No mode, no new key. The implementation ships only at its
cleanest shape: an explicit gesture lifecycle, no stored deferred state, and hit tests that index
what the renderer painted.

## Milestone Map

1. Gesture and surfaces: the full selection, copy, and gutter capability, built and reviewed.
   Ended on an information boundary: three review rounds showed the lifecycle frame was wrong.
2. Ideal rebuild: the re-derived lifecycle (complete on proof, zero stored deferrals) and the
   paint-time geometry recording. Ends at the merge gate.

## Current Milestone

`02-ideal-rebuild.md`

## Deferred Decisions

- `painted_sel` (`src/ui.rs`) rebuilds the painted render per call, unmemoized. Benched clean
  on the current scenarios. A cache follows the `row_cache` pattern when a bench run flags it.

## Replan

- 2026-08-19: /thebar reset after review round 3 → a fresh judge re-derived the lifecycle →
  milestone 2 opened, `plan.md` renamed to `01-gesture-and-surfaces.md`.
- 2026-08-18: initial strategy, single milestone.
