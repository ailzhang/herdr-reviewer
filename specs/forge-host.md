---
Status: Current
Created: 2026-06-27
Last edited: 2026-08-15
---

# forge host

The `PR` tab shows the pull request for the branch that you are on. The tab shows the state, the checks, and the comments. reviewr reads the pull request through the CLI of the forge. reviewr does not write to the forge. How the tab shows the data is in `pr-tab.md`.

## Overview

reviewr finds the PR of the branch. Then reviewr reads a snapshot of that PR on each poll. The tab follows one PR from open through merged. Then the tab switches to the next PR of the branch. If there is no PR, the tab is empty.

The hostname of the remote selects the forge. GitHub is read with `gh`. GitLab is read with `glab`. Azure DevOps is read with `az`. Differences per forge are in `forge-providers.md`. The rules below hold for all three.

```
PR #226  open  persiyanov/deep-research-benchmark → main   ⇡ 2 unpushed
  merge      ⚠ conflicts with main
  checks     ✗ failing — ✓ build-main-image · ✓ review · ✗ tests
  comments   5 (newest first) — @you 5m · @codex 2h · @claude 2h · …
```

The snapshot:

| field          | type   | meaning                                                                |
| -------------- | ------ | ---------------------------------------------------------------------- |
| `number`       | int?   | PR number, `null` when no PR is found                                  |
| `title`, `url` | string | identity                                                               |
| `body`         | string | the PR description as the forge returns it, empty when there is none   |
| `state`        | enum   | `open`, `merged`, or `closed`                                          |
| `is_draft`     | bool   | draft flag                                                             |
| `head_ref`     | string | the head branch name of the PR, which can differ from the local branch |
| `head_is_fork` | bool   | the head is in a different repository                                  |
| `base_ref`     | string | the merge target                                                       |
| `merge`        | enum   | `clean`, `conflicting`, or `blocked`                                   |
| `sync`         | enum   | `in_sync`, `unpushed`, `behind`, or `unknown`, with a count when known |
| `checks`       | list   | one row per latest check: `name` and `status` (conclusion is in it)    |
| `comments`     | list   | one row per comment, newest first                                      |
| `truncated`    | bool   | a limited surface had one more page, so a list is a prefix             |

A `comments` row:

| field                        | type         | meaning                                                                          |
| ---------------------------- | ------------ | -------------------------------------------------------------------------------- |
| `kind`                       | enum         | `review` (a review body), `comment` (conversation), `finding` (inline)           |
| `author`, `author_is_bot`    | string, bool | the `@login` and whether the author is a bot                                     |
| `anchor`                     | string       | `path:line` or `path:start-end` for a `finding`, the kind word in other cases    |
| `place`                      | object or none | path, range, and side for a `finding`, none in other cases                     |
| `body`, `snippet`            | string       | the text as the forge returns it, only a `finding` has a snippet                 |
| `created_at`                 | time         | post time, the newest-first sort key                                             |
| `is_resolved`, `is_outdated` | bool         | thread state for a `finding`, always false in other cases                        |
| `reply_count`                | int          | replies on a `finding` thread after the root                                     |

## Behavior

### Forge hosts

Each forge knows its public hosts. One config key per forge adds one self-hosted hostname (`config.md`). A key adds a host. A key does not remove a built-in host. The match does not use letter case.

| forge        | built-in hosts                        | self-hosted key     |
| ------------ | ------------------------------------- | ------------------- |
| GitHub       | `github.com`                          | `github_host`       |
| GitLab       | `gitlab.com`                          | `gitlab_host`       |
| Azure DevOps | `dev.azure.com`, `*.visualstudio.com` | `azure_devops_host` |

A remote counts when its hostname matches a forge host and its path is a repository on that forge. `upstream` wins over `origin`. A fork clone reads the PRs of the base repository with no setup.

| remote state                                                           | outcome                                                                     |
| ---------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| `upstream` names a recognized forge host with a repository identity    | reviewr reads that repository on that forge                                 |
| `upstream` is absent, has no host, is not supported, or is not correct | `origin` selects the repository                                             |
| reading `upstream` fails                                               | reviewr shows the Git error that you can retry, and does not fall through   |
| `origin` names a recognized forge host with a repository identity      | reviewr reads that repository on that forge                                 |
| `origin` names a different hosted repository                           | reviewr names the host that is not supported and points to the host keys    |
| `origin` is missing or has no host                                     | reviewr says the PR tab needs a recognized forge `upstream` or `origin`     |
| `origin` names a recognized host without a repository identity         | reviewr says the forge origin is not correct                                |
| reading `origin` fails                                                 | reviewr shows the Git error that you can retry                              |

The repository target is the forge, the hostname, and the repository path together. reviewr reads the fetch URL of each remote after the `insteadOf` rewrite of Git.

A push URL does not select a repository. An SSH alias does not select a repository. A CLI variable such as `GH_HOST` does not select a repository. An Azure DevOps ssh host counts as the https host of that server.

An SSH remote works in the `git@host:path` form and in the `ssh://` form. A web remote works in the `http://` form, the `https://` form, and the `git://` form. A different scheme is not a repository.

### Resolution

The tab shows the newest PR that was opened from the current branch. If there is no branch, the tab is empty. If there is no PR, the tab is empty.

A PR can have a branch name that is not the local name. reviewr searches with as many as three names:

- the name of the branch
- the name of the tracked branch, if that name is not a resolved base and is not a recorded base
- an `origin` branch that points at the work of this branch

Tracking `main` is not a publication to `main`. A pick that does not resolve still names a base. `git push origin HEAD:other-name` still finds the PR. An `origin` branch that points at base history has no work. That branch does not count.

reviewr uses only the PRs in the selected repository.

- The newest open PR wins. An open PR matches by name only. A branch that has the same name as a busy branch uses the open PR of that name.
- If there is no open PR, the newest merged PR or closed PR wins. This holds only if this branch contains the head commit of that PR.
- A reused branch name does not bring back an old PR.
- A PR of a teammate from a different branch does not attach. This holds even if that PR uses commits of this branch.
- If there is no such PR, the tab is empty.

More rules:

- Each fetch sets `HEAD` and the base to fixed commits first. A commit by the agent during the fetch cannot change those commits.
- On a fork, reviewr also asks for the PRs of the fork when the query of the forge can reach them.
- A PR into upstream is above a PR of the fork. In upstream, only a PR from the fork counts.
- A detached `HEAD` has no branch. The tab shows the empty state. The tab does not remove a snapshot that is already on the screen.
- A pushed name depends on local records. A removed `origin/*` ref can hide a PR. A missing tracking record can hide a PR.

### Derived state

`merge` shows only blockers.

| condition                         | `merge`       |
| --------------------------------- | ------------- |
| a conflict                        | `conflicting` |
| a rule block or a policy block    | `blocked`     |
| a forge that still computes       | `clean`       |
| any other case                    | `clean`       |

The footer shows `clean` as nothing.

`sync` compares the pinned `HEAD` to the head commit of the PR.

| condition                         | `sync`                    |
| --------------------------------- | ------------------------- |
| the two commits are equal         | `in_sync`                 |
| `HEAD` is ahead                   | `unpushed`, with a count  |
| the PR head is ahead              | `behind`                  |
| the PR head is not on the machine | `unknown`                 |

`unknown` is never a guessed `in_sync`.

`unpushed` means the checks and comments on the screen describe a commit that is older than the local tree.

### Checks

- There is one row per check name. Only the latest run counts. A new passed run replaces the earlier failure.
- A top rollup gives the pass or the fail for the set.

### Comments

- Reviews, inline threads, and conversation comments join into one list. The newest comment is first.
- A finding location is a line range on a path. A thread on one line is a range of one line.
- The `anchor` is `path:start-end` when the two ends differ. The `anchor` is `path:line` when they do not.
- A thread with no line has an `anchor` that is only the path. That thread has no range.
- The PR-level posts of a bot collapse to the latest post. All posts of a human stay.
- `is_resolved` and `is_outdated` come from the forge. reviewr does not compute them again. Resolved threads and outdated threads stay in the list. The list marks them.
- Each surface reads its newest 100 rows. The surface does not page until the end. One more page sets `truncated`. `pr-tab.md` marks the limited list.
- If a forge cannot find its newest page, reviewr serves the oldest page. The list is marked truncated.

### Refresh

- The first fetch starts when the panel opens.
- A new fetch starts when the user enters the tab. A new fetch starts on the `refresh` key (default `r`). A new fetch starts when the worktree ends a turn (`herdr-host.md`, HH-TURN-PER-WORKTREE). An agent can push or merge with no local trace.
- On the tab, a fallback poll reads again every 60 seconds. Off the tab, there is no poll.
- One fetch runs at a time. `refresh` stops that fetch and starts again. A different trigger lets the fetch complete and show. Then reviewr runs one new fetch.
- A result shows only if all of its input still matches: config, repository target, branch, pinned commits, branch names. If the input does not match, reviewr drops the result and fetches again. This holds on the tab and off the tab.
- A commit or a push is freshness. A commit or a push is not identity. reviewr fetches again behind the snapshot that is on the screen. reviewr does not make that snapshot empty. The same PR with newer work is old, not wrong. The in-flight mark covers the gap (`tui.md`).
- The tab becomes empty only when the repository target, the origin, or the checked-out branch changes. Then reviewr cannot prove that the snapshot still belongs to this branch (`overview.md` Continuity).
- If a fetch finds no PR, reviewr keeps the snapshot while the pinned `HEAD` is or contains the head commit of the shown PR. A removed remote branch does not make the tab empty during the session. A pull of the merged base does.
- Each fetch uses one validated config snapshot for host selection and base selection (→ CFG-ONE-SNAPSHOT, `config.md`).
- Each fetch makes the snapshot again in full. There is no cache after what is on the screen.
- When reviewr exits, it stops the schedule and restores the terminal immediately. Nothing shows after that.

## Failure semantics

reviewr only reads. Each failure goes to a clear state. `Changes` and `All files` do not change.

- A failure on the same input keeps the snapshot that is on the screen and shows its remedy. If there is no snapshot, the remedy fills the tab.

| failure                                   | remedy shown                                         |
| ----------------------------------------- | ---------------------------------------------------- |
| missing forge CLI or required extension   | the install step of that part (`forge-providers.md`) |
| a fetch with no authentication            | the login command of that CLI (`forge-providers.md`) |
| any other fetch error                     | the retry error                                      |

- A failure before the repository target is found replaces any snapshot with the Git error that you can retry.
- A Git failure after the same target was found keeps the snapshot. The same error shows.
- An origin that is not a recognized forge, or that stops being one, replaces any snapshot with the host remedy that is not supported. The remedy points to the host keys.
- A host key that names a server that runs a different forge fails as the fetch error of the selected CLI.
- If there is no PR, the tab shows the calm empty state. The next poll fills the tab when a PR appears.
- Two active PR tabs on one worktree become the same within one poll interval.

## Non-goals

- reviewr does not write to a forge. reviewr does not post. reviewr does not resolve. reviewr does not run checks again. reviewr does not merge. reviewr does not send PR comments to the agent.
- reviewr has no transport of its own. The CLI of the forge owns hosts, credentials, and TLS.
- There is no repository selector. There is no search across repositories.
- Sibling worktrees from one clone do not have different parent repositories. Use a separate clone.
- There is no SSH host-alias change to a standard name. A repository that has only an alias needs a remote with a canonical host.
- reviewr does not find a publication name that is not recorded on a remote that is not `origin`.
- reviewr does not detect a rename or a redirect on the forge. reviewr trusts the remote identity as it is.
- The tracked branch name is not scoped to a remote. The bare name applies for each remote that it tracks.
- There is no event subscription. The snapshot polls the CLI. There is no webhook. There is no socket.
- There is no server-version layer for self-hosted schemas.

## Related specs

- [forge-providers](./forge-providers.md)
- [configuration](./config.md)
- [pr-tab](./pr-tab.md)
- [herdr-host](./herdr-host.md)
- [overview](./overview.md)
