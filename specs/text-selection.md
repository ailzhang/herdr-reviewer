---
Status: Current
Created: 2026-08-18
Last edited: 2026-08-20
---

# Text selection

Selecting visible text with the mouse and copying it to the clipboard.

## Overview

The reviewer drags over text and it is selected, character by character. Releasing the drag copies
the selection. The drag is the whole gesture: no mode, no prior keypress. Commenting by mouse
lives in the gutter (`input.md`).

```
 16 ▌ const style = token.h▓▓▓▓▓▓▓▓▓ ?? getTokenStyleObject(token);
 17 ▌ element.textContent = ▓▓▓▓▓▓▓▓▓▓▓▓▓;
      → release: footer shows `copied 23 chars`
```

## Selection

A selection starts at mouse-down on text. A gutter mouse-down starts the comment gesture
instead (`input.md`). Both follow the pointer until release, keep their mouse-down kind when
the pointer crosses the gutter-body seam, and end by the table below.

- It works on every body surface that paints file or forge text: the Diff view, the File view, the
  markdown preview, the `PR` tab's read pane, and both navigators (`file-list.md`, `pr-tab.md`).
  The overlays are excluded (Non-goals).
- A pointer outside the drag's surface clamps to its nearest cell. A mouse-down on blank space
  starts nothing.
- A release on the mouse-down character's cells is a click, and the click keeps its existing
  meaning (`input.md`). A release anywhere else ends the drag.
- It is a stream selection. On one row it runs start character to end character. Across rows
  the first row runs from its start character, whole rows lie between, and the last row runs
  up to its end character. The highlight appears at the first pointer motion. A press that
  never moved paints nothing.
- A double-click on a character surface selects the word under the cell and copies it. A
  word is an unbroken run of letters, digits, and underscores. On whitespace, punctuation,
  or past the text, the double acts as the click. A triple-click selects the row's whole
  source line and copies it, and on an empty line acts as the click. In a navigator, a
  double-click copies the row's text (Copy), a triple repeats it, and a row with nothing to
  copy takes the click instead. Further clicks within the window repeat the triple. Each
  multi-click fires at the release on the mouse-down cell, like a drag's, so a press inside
  the click window that drags away is a plain drag selection. The click chain resets when
  the cell maps to different content, by an edit or a scroll, when a click lands on another
  cell, when a drag completes, or when a gesture ends without its release.
- The copy leaves its span highlighted as feedback, on every surface alike. The settled
  highlight is anchored to its rows, so scrolling moves it with the content. It clears at
  the next mouse-down, at any keypress or resize, and when a refresh changes the text it
  spans. On
  the `PR` tab a landed snapshot replaces the whole paint, so any replace or clear blanks
  it, and a kept snapshot (a transient fetch error) keeps it. It is paint only: it cannot
  be extended, re-copied, or turned into a comment.
- A live gesture holds what it anchors to, exactly as composing holds the open diff
  (`overview.md` Continuity): a code, card, or preview drag holds the open view while the
  lists update, a navigator drag holds the file list, and a `PR` drag holds the fetched
  result. An abandoned gesture outlives the pointer's exit by at most the exit deadline.
- The wheel during a live gesture scrolls the pane and extends the selection, counting as
  motion. So does moving the pointer past the pane's content rows, onto the border or beyond.
  The outermost content rows are selectable without scrolling. With wrap off, the columns past
  the content scroll horizontally the same way, stopping at the widest visible row's last
  column.
- A cell inside a wide character or an expanded tab selects that whole character.
- Text selection stays available while the comment editor is open, so a reviewer can copy code
  into a draft. The frozen view under the editor is what it selects from.

How a gesture ends. Proof that the button is up completes a visible selection instead of
cancelling it. The proofs are pointer motion with no button held and the next mouse-down. Drag
to the pane's edge, release past it, and the copy still happens. A still pointer anywhere
inside the pane is a held button, because a release there would have arrived, so the gesture
waits. Stillness after the pointer's last event on the pane's own edge is the exit signature,
and the exit deadline completes the gesture.

| the gesture ends by                                        | press that never moved                 | drag with a selection          | gutter press or drag    |
| ---------------------------------------------------------- | -------------------------------------- | ------------------------------ | ----------------------- |
| its release                                                | the click, or the multi-click's action | the copy                       | the composer            |
| a release proof: buttonless motion or the next mouse-down  | nothing                                | the copy                       | nothing                 |
| the exit deadline: stillness after the pointer left        | nothing                                | the copy                       | nothing                 |
| a config layout or theme change                            | nothing                                | the copy                       | nothing                 |
| a reflow input: a keypress or a resize                     | nothing, the input acts                | nothing copies, the input acts | nothing, the input acts |

When end conditions coincide, the first matching row decides the gesture, and an input still
acts. The next mouse-down completes the old gesture first, then acts as any mouse-down. The
tick that completes a gesture lands what the freeze held, under the usual rules (`overview.md`
Continuity). A drag or a release with no live gesture does nothing. A navigator press that
never leaves its mouse-down row counts as a press that never moved, so a one-cell slip still
activates the row. `TS-NO-SILENT-LOSS` covers selections only: a lost gutter drag dissolves,
and the composer never opens unasked.

The invariants:

| code                 | Always true                                                                        |
| -------------------- | ---------------------------------------------------------------------------------- |
| `TS-ONE-SURFACE`     | A selection never leaves the pane and the content kind where it started.           |
| `TS-NO-REVIEW-STATE` | A text selection never creates, moves, or removes a comment or a line selection.   |
| `TS-NO-SILENT-LOSS`  | Only the user's own input ends a visible selection without its copy.               |

`TS-ONE-SURFACE`: the pane is a navigator or the read pane. The content kind is file text or
comment-card text: a drag that starts on code skips spliced comment cards, and a drag that starts
on a card selects that card's text. In the `PR` read pane, all painted text is one kind.

## Copy

Releasing the drag writes the selection to the system clipboard and shows a `copied N chars`
status in the footer (`input.md`), never the bare `copied` of the comment export. The count
is characters of the copied text, and the status pluralizes `char`.

The clipboard receives source text, never painted chrome:

- Each spanned row contributes its underlying text once: whole rows in full, the first and last
  rows cut at the selection's ends. The rows join with `\n`.
- A wrapped line contributes one line, not one per display row.
- In the Diff and File views, a row's text is its source line. In rendered markdown and the `PR`
  tab's read pane, it is the painted text of the block, snippet rows included.
- A file-navigator row contributes its full repo-relative path, the directories the tree
  nests it under included. A directory row contributes the directory's own path. A
  `PR`-navigator row contributes its text in full, even when the pane truncates it. All
  exclude the tree glyphs and annotations.
- Gutter cells, fold rows, notices, card borders, trailing pad columns, and full-width
  separator rules are chrome, and contribute nothing.
- Spanned comment cards contribute nothing (→ TS-ONE-SURFACE).

The write goes through the platform clipboard tool chain, and a failure there reports loudly
(`herdr-host.md`).

## Non-goals

- No clipboard over SSH. `herdr-host.md` owns that exclusion, and OSC 52 stays on the roadmap.
- No host protocol change. A herdr-forwarded release or pane-leave event would replace the
  exit deadline, and stays on the roadmap.
- No copy-on-select toggle. The copy on release is the one behavior.
- No selection in the overlays: the search screen, the find band, the comments list, the pickers,
  and the comment editor.
- No select-all.

## Related specs

- [input](./input.md)
- [diff-view](./diff-view.md)
- [herdr-host](./herdr-host.md)
- [overview](./overview.md)
