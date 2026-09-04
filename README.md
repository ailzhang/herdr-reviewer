# herdr-reviewr

> [!IMPORTANT]
> **This is a manual fork of [persiyanov/herdr-reviewr](https://github.com/persiyanov/herdr-reviewr)
> by [Dmitry Persiyanov](https://github.com/persiyanov), who wrote essentially all of it.**
> All credit for the original work is theirs. MIT licensed, copyright © 2026 Dmitry Persiyanov
> — see [LICENSE](LICENSE).
>
> "Manual" means it was copied, not forked through GitHub, so **GitHub shows no link between the
> two repos**. Nothing warns you when you aim at the wrong one. Before you push, open a PR, or
> file an issue, check where it is going:
>
> ```
> ailzhang/herdr-reviewer   ← this repo, private, the Sapling port lives here
> persiyanov/herdr-reviewr  ← upstream, public, git-only. Never push the Sapling work here.
> ```
>
> Note the spelling: this repo ends in **-er**, upstream in **-r**. That is not a typo.

A code-review pane for [herdr](https://herdr.dev). Your agent writes the code. You read its
diff in a pane beside the chat, comment on the lines, and send the notes back. You never leave
the terminal.

This fork adds **Sapling (`sl`)** support on top of upstream, and publishes no release
binaries — you build it here.

![demo](assets/demo.gif)

<p align="center">
  <a href="#install-from-this-repo">install</a> ·
  <a href="#shortcuts">shortcuts</a> ·
  <a href="#scopes-and-the-base-picker">scopes</a> ·
  <a href="#sapling-worktrees">sapling</a> ·
  <a href="#configuration">configuration</a> ·
  <a href="CHANGELOG.md">changelog</a>
</p>

It never edits your worktree and sends nothing on its own. In a git repository its only writes
are private refs under `refs/reviewr/`; in a Sapling repository it writes nothing inside the
repo at all.

## Install from this repo

Requirements: **herdr ≥ 0.7.5**, **git** (and `sl` for Sapling repos) and **`jq`** on `PATH`, a
**Rust toolchain**, a **truecolor** terminal, **macOS or Linux**. The **PR** tab additionally
needs an authenticated `gh`, `glab`, or `az`. `jq` is easy to miss — the pane actions in
`herdr/pane.sh` parse every herdr API response with it, so without it the pane never opens.

```bash
git clone https://github.com/ailzhang/herdr-reviewer
cd herdr-reviewer
just install          # cargo build --release → bin/herdr-reviewr (re-signed on macOS)
herdr plugin link .
```

Confirm the link took, because the source string is what decides whether your local rebuilds
ever reach a pane:

```bash
herdr plugin list     # want: persiyanov.reviewr (reviewr) enabled [local:/path/to/herdr-reviewer]
```

No `just`? `cargo build --release && ./scripts/swap-binary.sh target/release/herdr-reviewr bin/herdr-reviewr`
does the same thing. Copying onto an existing `bin/herdr-reviewr` in place is what
`swap-binary.sh` avoids: macOS SIGKILLs a binary whose inode was overwritten.

Do **not** run `herdr plugin install ailzhang/herdr-reviewer`. That path executes
`herdr/install.sh`, which downloads a prebuilt binary from upstream's releases — upstream is
git-only, so you would silently land on a build with no Sapling support. `herdr plugin list`
showing a `github:…` source means local rebuilds never reach your panes; fix it with:

```bash
herdr plugin uninstall persiyanov.reviewr   # config is keyed by plugin id and survives
herdr plugin link .
```

Open the pane in the current workspace (the plugin id is unchanged from upstream):

```bash
herdr plugin action invoke open --plugin persiyanov.reviewr
```

That command only works from inside herdr — the action reads its target workspace from the
environment herdr injects, and refuses with `no workspace context` anywhere else.

reviewr also auto-opens in new worktrees. Bind the toggle in your **herdr** config (the user
config, not the plugin manifest):

```toml
[[keys.command]]
key = "cmd+r"          # on Linux, prefer "prefix+r"
type = "plugin_action"
command = "persiyanov.reviewr.toggle"   # <plugin_id>.<action_id> — the id, not the name
```

`cmd+…` chords reach herdr; many macOS terminals swallow `alt+…` themselves. Linux terminals
generally do not deliver `cmd+…` at all, so bind through the herdr prefix instead.

A new binding needs no restart, but it does need a reload:

```bash
herdr config check            # want: config: ok
herdr server reload-config    # want: "status":"applied"
```

**To update:** `git pull && just install`, then toggle the pane off and on. An open pane keeps
running the old process — a refresh inside reviewr will not pick up a new binary.

**Without herdr**, reviewr runs as a plain terminal app. Everything works except **Send** and
the **last turn** scope, which need herdr around:

```bash
cargo run --release -- ~/some/repo
```

### If a pane action does nothing

Pane actions fail silently in the UI: a broken action looks exactly like an unbound key. The
reason is always in the plugin log, so start there rather than guessing:

```bash
herdr plugin log --plugin persiyanov.reviewr
```

An entry with `exit_code: 1` means the key *is* bound and the action ran — read its `stderr`.
No new entry at all means the keybinding never reached herdr.

One failure worth naming, since the log line (`herdr pane list failed for <ws>`) does not
point at it: herdr passes the plugin its own binary path in `HERDR_BIN_PATH`, read from
`/proc/self/exe`. A server whose binary was replaced on disk since it started — any herdr
self-update — reports that as `/path/to/herdr (deleted)`, which cannot be executed, and every
action that calls back into herdr fails. `pane.sh` falls back to a `PATH` lookup so reviewr
keeps working, but other plugins may not; restart the herdr server when convenient. Note that
`herdr status` still reports `restart_needed: no` in this state.

## Quick start

1. **Pick a file.** Changed files are in the navigator. `j` / `k` moves, the diff follows. Or
   `]` walks the changes hunk by hunk, file after file.
2. **Focus the diff.** `Tab` switches panes.
3. **Select lines.** `v`, then `j` / `k` to extend (or click or drag the gutter).
4. **Comment.** `c`, type, `Enter`.
5. **Send.** `s` sends every comment to the agent's input, then clears them.

The footer names the next step. Press `?` for every key that works right now.

## Shortcuts

The keys below are defaults. Every action in the last column is rebindable, to one key or
several ([Keybindings](#keybindings)). `Tab`, `Esc`, and `Enter` are structural and fixed.

**Getting around**

| Key | Does | Action name |
| --- | --- | --- |
| `1` `2` `3` | Switch tab — Changes / All files / PR | `tab-changes` `tab-all-files` `tab-pr` |
| `u` `b` `t` | Switch scope — uncommitted / branch / last turn | `scope-uncommitted` `scope-branch` `scope-last-turn` |
| `B` | Pick the branch scope's base | `base-pick` |
| `j` `k` · `↑` `↓` | Move the cursor | `down` `up` |
| `]` `[` | Next / previous hunk | `next-hunk` `prev-hunk` |
| `f` `F` | Next / previous file | `next-file` `prev-file` |
| `PageUp` `PageDown` | Move a page | `page-up` `page-down` |
| `Ctrl+U` `Ctrl+D` | Move a half page | `half-up` `half-down` |
| `Tab` | Switch focus between navigator and diff | — |
| `→` `←` | Expand / collapse, else scroll sideways | `expand` `collapse` |
| `/` | Search files and code | `search` |
| `Ctrl+F` | Find in the open file | `find` |
| `w` | Toggle line wrap | `wrap` |
| `m` | Toggle markdown preview | `preview` |
| `p` | Rotate the navigator clockwise | `navigator-position` |
| `z` | Hide / show the navigator | `navigator-hide` |
| `<` `>` | Grow / shrink the navigator | `navigator-grow` `navigator-shrink` |
| `r` | Refresh now | `refresh` |
| `?` | Expand the footer to every shortcut | `keys` |
| `q` | Quit | `quit` |

**Reviewing** (in the diff)

| Key | Does | Action name |
| --- | --- | --- |
| `v` | Select a line range | `select` |
| `c` | Comment on the line or selection | `comment` |
| `e` `d` | Edit / delete the comment under the cursor | `edit` `delete` |
| `n` `N` | Next / previous comment | `next-comment` `prev-comment` |
| `l` | List and manage all comments | `comments` |
| `s` | Send every comment to the agent | `send` |
| `y` | Copy every comment to the clipboard | `copy` |
| `Esc` | Clear the selection | — |

**In the comment box** (and the search input and the picker filters)

| Key | Does |
| --- | --- |
| `Enter` | Save |
| `Esc` | Cancel, discarding the draft |
| `Shift+Enter` · `Alt+Enter` · `Ctrl+J` | Insert a newline |
| `←` `→` · `↑` `↓` | Move the caret by character / wrapped row |
| `Home` `End` · `Ctrl+A` `Ctrl+E` | Start / end of the line |
| `Alt+b` `Alt+f` | Move by a word |
| `Ctrl+W` · `Ctrl+U` `Ctrl+K` | Delete the word before · to line start / end |

**In the base picker**

| Key | Does |
| --- | --- |
| typed character | Filter the rows, matching anywhere in the name |
| `↑` `↓` · `Ctrl+P` `Ctrl+N` | Move the highlight |
| `Enter` | Pick the row, or resolve what you typed as a revision |
| `Esc` | Cancel |

The comments list and the agent picker move with your normal `j` / `k` bindings and close on
`Esc`.

**PR tab** (read-only)

| Key | Does |
| --- | --- |
| `j` `k` | Move through description and comments |
| `PageUp` `PageDown` | Scroll the focused pane |
| `o` | Open the PR in the browser |
| `r` | Refresh |

**Mouse.** Drag over any text to select and copy it, double-click a word, triple-click a line.
Click or drag the line-number gutter to comment. Click files, tabs, the scope chip, the base
name, and links. Drag the divider to resize. Scroll with the wheel.

## Scopes and the base picker

- **uncommitted** — the working tree vs `HEAD` (staged, unstaged, and untracked).
- **branch** — the working tree vs the merge base with the base branch: uncommitted work plus
  the branch's commits.
- **last turn** — everything that changed in this worktree since its most recent agent turn
  started. Needs herdr.

reviewr starts in **uncommitted**; `default_scope` changes that, and `u` / `b` / `t` wins for
the session. Every scope respects `.gitignore`, so build output never clutters **Changes**.

The **branch** base is your repo's default branch (`origin/HEAD`), shown in the header as
`vs main`. Press `B` (or click the base name) to pick another branch, or type any revision —
`HEAD~2`, a tag, a SHA prefix. The pick is stored in the repo and holds until you pick again;
choosing the default branch clears it. A pick that has gone away is skipped, and the header
says so: `vs main · dev missing`. `--base <ref>` pins the base for one pane and disables the
picker.

## Sapling worktrees

A pane detects git or Sapling once, at open. The review loop is identical; the deltas
([specs/sapling.md](specs/sapling.md)):

- **Only the Changes tab**, because **All files** and the **PR** tab would each enumerate a
  monorepo. Both are hidden, so the tab bar is just `Changes`. Search is off too, and says so
  in one calm line.
- **`HEAD` is `.`**, the working-copy parent, and there is no index — the working copy is the
  only uncommitted state.
- **The default base** is the last public ancestor, `last(public() & ::.)`.
- **The base picker lists your stack**: `whole stack` first, then local bookmarks, then `.` and
  its draft ancestors, newest first. Filter by description or hash.
- **Picking a commit reviews that commit alone**, against its own parent — the header reads
  `vs f01c3d2 → 2eb84b9` and uncommitted edits stay out of the view. Pick `whole stack` to go
  back to the public base.
- **The send names that commit.** Because the reviewed state isn't the working copy, the export
  leads with `reviewing commit 2eb84b9, not the working copy`, so the agent knows where the
  fix belongs. Every other scope reviews the worktree the agent edits, and adds no such line.
- **Nothing is written inside the repo.** The base pick and turn baselines live in
  `~/.local/state/herdr-reviewr/sl/<worktree>/`. Deleting that directory loses the pick and the
  baseline, never a comment.

## Configuration

CLI flags on the pane command:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--poll <ms>` | `2000` | worktree poll interval (min `200`) |
| `--base <ref>` | auto | base for `branch` scope, any rev, overrides the pick |
| `--theme <name>` | `catppuccin` | UI + syntax theme |
| `--wrap <on\|off>` | `on` | soft-wrap long diff lines (`w` toggles at runtime) |

Everything else lives in reviewr's own config file — create it if missing. Settings in herdr's
`~/.config/herdr/config.toml` never reach it, and reviewr re-reads it on every refresh and
toggle:

```text
~/.config/herdr/plugins/config/persiyanov.reviewr/config.toml
```

```toml
theme = "tokyo-night"            # 18 palettes, dark and light — see below
default_scope = "branch"         # uncommitted | branch | last-turn
navigator_position = "right"     # right | bottom | left | top
toggle_placement = "split"       # split | overlay | zoomed | tab
toggle_direction = "right"       # right | down — split only
auto_open = true                 # false when a layout plugin arranges your worktrees
github_host = "github.example.com"   # also gitlab_host, azure_devops_host

[keybindings]
comment = ["c", "ㅊ"]
select  = ["v", "ㅍ"]
```

A missing file or omitted key uses its default. An invalid file is rejected whole — the pane
shows the error and recovers on the next refresh after you fix it.

**Themes.** One theme colors chrome and syntax together. Match your terminal's background.
Dark: `catppuccin`, `catppuccin-frappe`, `catppuccin-macchiato`, `dracula`, `nord`, `gruvbox`,
`one-dark`, `solarized`, `monokai`, `tokyo-night`, `rose-pine`. Light: `catppuccin-latte`,
`gruvbox-light`, `one-light`, `solarized-light`, `github-light`, `tokyo-night-day`,
`rose-pine-dawn`.

### Keybindings

`[keybindings]` maps an action name from the [Shortcuts](#shortcuts) tables to an array of
keys. The array replaces that action's defaults, actions you don't mention keep theirs, and
hints show the first key:

```toml
[keybindings]
comment = ["c", "ㅊ"]
select  = ["v", "ㅍ"]
```

Several keys per action serves CJK input sources — bind the character your layout produces on
the same physical key. A key is one printable character or a `ctrl+`/`alt+` chord. Keys still
type normally in the comment box. Two actions sharing a key invalidates the file.

## Good to know

- **Comments are in-memory and single-session.** Closing the pane loses any you haven't sent or
  copied out. Sending is all-or-nothing: it delivers the whole set and clears it, and a failure
  leaves everything in place.
- **No line-number rebasing.** A comment stays locatable by its diff snippet. reviewr flags a
  stale comment instead of dropping it.
- **Send needs an agent in the workspace.** One agent takes the comments straight away; several
  open a picker.
- **last turn relies on polling** (2 s default). A turn that starts and finishes inside one
  poll is missed.
- **The PR tab is read-only** and needs `gh`, `glab`, or `az` plus a recognized `upstream` or
  `origin`. It mirrors the branch's open PR or MR, capped at the newest 100 rows per surface.
- **Truecolor and box-drawing glyphs are required**; there is no 256-color fallback and no
  light/dark autodetect. Files over 2 MB or 50,000 lines show a "too large" notice.

## Design and development

The living design is in [`specs/`](specs/), one concept per doc, always current
([overview.md](specs/overview.md) is the map). For the dev setup, tests, and benchmarks see
[CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE). Syntax highlighting comes from [syntect](https://github.com/trishume/syntect)
and [two-face](https://github.com/CosmicHorrorDev/two-face). Bundled `.tmTheme` files in
`assets/`, each under its own license: [Catppuccin Mocha](https://github.com/catppuccin/bat)
(MIT), [Tokyo Night](https://github.com/folke/tokyonight.nvim) (Apache-2.0),
[Rosé Pine](https://github.com/rose-pine/tm-theme) (MIT).
</content>
</invoke>
