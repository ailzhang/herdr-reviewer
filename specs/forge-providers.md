---
Status: Current
Created: 2026-07-22
Last edited: 2026-08-15
---

# forge providers

What is different per forge behind `forge-host.md`: the repository identity, the CLI, and how the concepts of each forge fill the one snapshot.

## Overview

| forge        | CLI                                 | noun          | abbreviation | reference |
| ------------ | ----------------------------------- | ------------- | ------------ | --------- |
| GitHub       | `gh`                                | pull request  | `PR`         | `#226`    |
| GitLab       | `glab`                              | merge request | `MR`         | `!42`     |
| Azure DevOps | `az` + the `azure-devops` extension | pull request  | `PR`         | `#12`     |

On the screen, only the words differ. Each forge has its name, its noun, its abbreviation, and its reference form (`pr-tab.md`). Branch names go to each CLI as they are.

Each provider owns its full read. An optional surface that cannot be read adds nothing. That surface does not fail the fetch. A mapping that is not stated below is the identity.

## GitHub

- Identity is `owner/repository`.
- The CLI is `gh`. The login remedy is `gh auth login --hostname <host>`.
- `CONFLICTING` or `DIRTY` is `conflicting`. `BLOCKED` is `blocked`. All other values are `clean`.
- `UNKNOWN` means GitHub still computes. Then `merge` is `clean`, unless `mergeStateStatus` is `DIRTY`.
- Checks are check runs and commit statuses. There is one list.
- A review is a `review` row. A review thread is a `finding` row. The row has the resolved flag and the outdated flag from GitHub. A conversation comment is a `comment` row.
- The query finds PRs by head branch name in the target repository. Open PRs come on their own page beside the finished page. A long finished history cannot hide an open PR.
- On a fork, an upstream result counts only when its head is in the fork. A merged PR or a closed PR whose fork was deleted still counts if the containment check confirms it.

## GitLab

- Identity is the full namespace path. The path can have more than two segments.
- The CLI is `glab`. The login remedy is `glab auth login --hostname <host>`.
- `opened` is `open`. `merged` is `merged`. `closed` and `locked` are `closed`. A cross-project MR sets `head_is_fork`.
- A conflict is `conflicting`. Blocking discussions, missing required approvals, or a denied policy are `blocked`. All other cases are `clean`. A check that still runs is `clean`.
- Checks are the jobs of the head pipeline. There is one row per job. A job that is allowed to fail counts as skipped. That job is never failing.
- If a jobs page is after the cap, reviewr adds one `pipeline` row with the verdict of the pipeline. If there is no pipeline, the list is empty. If the user cannot read the pipeline, the list is empty.
- An MR note is a `comment` row. A diff discussion is a `finding` row. The row has the resolved flag. There is no snippet. GitLab sends no code context. An approval is a `review` row.
- If the approvals surface cannot be read, reviewr adds no `review` rows.
- A service account counts as a bot. The names are `project_…_bot…`, `group_…_bot…`, and names that end with `[bot]` or `-bot`.
- After the GitLab count limit of about 10,000 rows, reviewr serves the oldest page. The list is marked truncated.
- The query finds MRs by `source_branch` in the target project. Opened MRs come on their own page beside the all-state page. A long finished history cannot hide an opened MR.
- On a fork, an upstream result counts only when its source project is the fork.

## Azure DevOps

- Identity is `organization/project/repository`.

| accepted URL form                                                                 | note                            |
| --------------------------------------------------------------------------------- | ------------------------------- |
| `dev.azure.com/{organization}/{project}/_git/{repository}`                        | https                           |
| `ssh.dev.azure.com:v3/{organization}/{project}/{repository}`                      | ssh                             |
| `{organization}.visualstudio.com` and `vs-ssh.visualstudio.com:v3`                | the old forms of the same hosts |

- reviewr removes a legacy `DefaultCollection` segment. A repository that has the same name as its project can omit the project segment. Names go in the URL with percent encoding. reviewr addresses the names after decode.
- The CLI is `az` with the `azure-devops` extension. A missing extension shows its install step. The login remedy is `az login`. For a personal access token the remedy is `az devops login`.
- `active` is `open`. `completed` is `merged`.
- A conflict is `conflicting`. A rejected required policy is `blocked`. All other cases are `clean`. A merge check that is still in the queue is `clean`.
- Checks are policy evaluations and commit statuses. There is one list.
- A PR-level thread is a `comment` row. A file-position thread is a `finding` row. The row has the resolved status of the thread. There is no snippet. A reviewer vote is a `review` row.
- An Azure service identity or build-service identity counts as a bot. The shared name suffixes also count as bots.
- The query finds PRs by `sourceRefName`. The query uses the newest 100 active PRs and the newest 100 completed PRs. The query is in the target repository only.
- A fork PR into the target is found through `forkSource`. That PR counts only when the pinned `HEAD` contains its source tip.
- The query does not include a fork's own internal PRs. The query does not include a merged PR that is older than the completed window. The query does not include an abandoned PR.
- An abandoned PR never counts as closed history. If the list cannot be read, the fetch fails.

## Non-goals

- There is no forge after these three.
- There is no show path per forge. The `PR` tab shows only the snapshot.

## Related specs

- [forge-host](./forge-host.md)
- [configuration](./config.md)
- [pr-tab](./pr-tab.md)
