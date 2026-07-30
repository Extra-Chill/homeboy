<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy git` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/git.md](../../../commands/git.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy git`

```sh
homeboy git <COMMAND>
```

Git operations for components

| Subcommand | Summary |
| --- | --- |
| `homeboy git status` | Show git status for a component |
| `homeboy git commit` | Commit changes (by default stages all, use flags for granular control) |
| `homeboy git push` | Push local commits to remote |
| `homeboy git rebase` | Rebase the current branch onto another ref |
| `homeboy git cherry-pick` | Cherry-pick one or more commits onto the current branch |
| `homeboy git pull` | Pull remote changes |
| `homeboy git tag` | Create a git tag |
| `homeboy git issue` | Manage GitHub issues for a component |
| `homeboy git pr` | Manage GitHub pull requests for a component |

## `homeboy git status`

```sh
homeboy git status [OPTIONS] [COMPONENT_ID]
```

Show git status for a component

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID]` | no | Component ID (non-JSON mode). When omitted, the component is auto-detected from CWD via the registry or a portable `homeboy.json` |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON input spec for bulk operations. Use "-" for stdin, "@file.json" for file, or inline JSON string |
| `--path` | `<PATH>` | Workspace path to operate on directly. Useful for unregistered checkouts (CI runners, ad-hoc clones, worktrees) |

## `homeboy git commit`

```sh
homeboy git commit [OPTIONS] [COMPONENT_ID] [SPEC]
```

Commit changes (by default stages all, use flags for granular control)

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID]` | no | Component ID (optional if provided in JSON body or auto-detected from CWD) |
| `[SPEC]` | no | Commit message or JSON spec (auto-detected). Plain text: treated as commit message. JSON (starts with { or [): parsed as commit spec. @file.json: reads JSON from file. "-": reads JSON from stdin |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | Explicit JSON spec (takes precedence over positional) |
| `-m`, `--message` | `<MESSAGE>` | Commit message (CLI mode) |
| `--staged-only` | flag | Commit only staged changes (skip automatic git add) |
| `--files` | `<FILES>` | Stage and commit only these specific files |
| `--exclude` | `<EXCLUDE>` | Stage all files except these (mutually exclusive with --files) |
| `--include` | `<INCLUDE>` | Explicit include list (repeatable) |
| `--path` | `<PATH>` | Workspace path to operate on directly. Useful for unregistered checkouts (CI runners, ad-hoc clones, worktrees) |

## `homeboy git push`

```sh
homeboy git push [OPTIONS] [COMPONENT_ID]
```

Push local commits to remote

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID]` | no | Component ID (non-JSON mode). When omitted, the component is auto-detected from CWD via the registry or a portable `homeboy.json` |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON input spec for bulk operations. Use "-" for stdin, "@file.json" for file, or inline JSON string |
| `--tags` | flag | Push tags as well |
| `--force-with-lease` | flag | Use `--force-with-lease` for safe force-pushes (e.g. after a rebase). Refuses to overwrite the remote if it has commits the local ref hasn't seen. Plain `--force` is intentionally not exposed |
| `--remote-url` | `<URL>` | Push to this remote URL directly instead of the configured upstream. Prefer this without credentials plus --token; embedding tokens in the URL can expose them in process listings |
| `--token` | `<TOKEN>` | GitHub token to inject into --remote-url for this invocation. Requires a https://github.com/... remote URL |
| `--refspec` | `<REFSPEC>` | Explicit push refspec, e.g. HEAD:refs/heads/my-branch |
| `--strip-extraheader` | flag | Clear GitHub Actions checkout's auth extraheader so URL auth wins |
| `--path` | `<PATH>` | Workspace path to operate on directly. Useful for unregistered checkouts (CI runners, ad-hoc clones, worktrees) |

## `homeboy git rebase`

```sh
homeboy git rebase [OPTIONS] [COMPONENT_ID]
```

Rebase the current branch onto another ref.

Default (no `--onto`) rebases onto the current branch's tracked upstream (`@{upstream}`), same semantics as `git pull --rebase`. Git's default rebase drops commits whose patch-id matches a commit already in upstream — squash-merged PRs are NOT dropped (different patch-id); that case will land in a follow-up.

On conflict, the operation returns a failed result with git's stderr. Resolve manually, then re-run with `--continue` or `--abort`.

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID]` | no | Component ID. When omitted, auto-detected from CWD |

| Option | Value | Description |
| --- | --- | --- |
| `--onto` | `<REF>` | Target ref to rebase onto. Defaults to the current branch's tracked upstream (`@{upstream}`) |
| `--continue` | flag | Continue an in-progress rebase after manual conflict resolution. Mutually exclusive with `--abort` |
| `--abort` | flag | Abort an in-progress rebase and return to the pre-rebase state |
| `--path` | `<PATH>` | Workspace path to operate on directly |

## `homeboy git cherry-pick`

```sh
homeboy git cherry-pick [OPTIONS] [REF]...
```

Cherry-pick one or more commits onto the current branch.

Accepts SHAs, branch names, and ranges (`<a>..<b>`) as positional args. Use `--pr <n>` to pick all commits from a GitHub PR via `gh`. Both can be combined.

On conflict, returns a failed result. Resolve manually, then re-run with `--continue` or `--abort`.

| Argument | Required | Description |
| --- | --- | --- |
| `[REF]...` | no | Commit refs to pick: SHAs, branches, ranges (`<a>..<b>`). Multiple positional args allowed |

| Option | Value | Description |
| --- | --- | --- |
| `-c`, `--component-id` | `<COMPONENT_ID>` | Component ID. When omitted, auto-detected from CWD |
| `--pr` | `<NUMBER>` | Cherry-pick all commits from a GitHub PR (repeatable). Resolved via `gh pr view <n> --json commits` |
| `--continue` | flag | Continue an in-progress cherry-pick after manual conflict resolution. Mutually exclusive with `--abort` |
| `--abort` | flag | Abort an in-progress cherry-pick |
| `--path` | `<PATH>` | Workspace path to operate on directly |

## `homeboy git pull`

```sh
homeboy git pull [OPTIONS] [COMPONENT_ID]
```

Pull remote changes

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID]` | no | Component ID (non-JSON mode). When omitted, the component is auto-detected from CWD via the registry or a portable `homeboy.json` |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON input spec for bulk operations. Use "-" for stdin, "@file.json" for file, or inline JSON string |
| `--path` | `<PATH>` | Workspace path to operate on directly. Useful for unregistered checkouts (CI runners, ad-hoc clones, worktrees) |

## `homeboy git tag`

```sh
homeboy git tag [OPTIONS] [COMPONENT_ID] [TAG_NAME]
```

Create a git tag

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID]` | no | Component ID. When omitted, the component is auto-detected from CWD via the registry or a portable `homeboy.json` |
| `[TAG_NAME]` | no | Tag name (e.g., v0.1.2) |

| Option | Value | Description |
| --- | --- | --- |
| `-m`, `--message` | `<MESSAGE>` | Tag message (creates annotated tag) |
| `--path` | `<PATH>` | Workspace path to operate on directly. Useful for unregistered checkouts (CI runners, ad-hoc clones, worktrees) |

## `homeboy git issue`

```sh
homeboy git issue <COMMAND>
```

Manage GitHub issues for a component

| Subcommand | Summary |
| --- | --- |
| `homeboy git issue create` | Create a new issue |
| `homeboy git issue comment` | Comment on an existing issue |
| `homeboy git issue find` | Find issues matching filters (dedup primitive) |
| `homeboy git issue close` | Close an existing issue with a typed reason |
| `homeboy git issue edit` | Edit an existing issue's title, body, or labels |

## `homeboy git issue create`

```sh
homeboy git issue create [OPTIONS] <COMPONENT_ID>
```

Create a new issue

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `-t`, `--title` | `<TITLE>` | Issue title |
| `-b`, `--body` | `<BODY>` | Issue body (markdown). Prefer --body-file for long content |
| `--body-file` | `<PATH>` | Read body from a file ("-" for stdin) |
| `-l`, `--label` | `<LABEL>` | Issue label (repeatable) |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json (for unregistered checkouts — CI runners, ad-hoc clones) |

## `homeboy git issue comment`

```sh
homeboy git issue comment [OPTIONS] <COMPONENT_ID>
```

Comment on an existing issue

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `-n`, `--number` | `<NUMBER>` | Issue number |
| `-b`, `--body` | `<BODY>` | Comment body (markdown). Prefer --body-file for long content |
| `--body-file` | `<PATH>` | Read body from a file ("-" for stdin) |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git issue find`

```sh
homeboy git issue find [OPTIONS] <COMPONENT_ID>
```

Find issues matching filters (dedup primitive)

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `-t`, `--title` | `<TITLE>` | Exact title match |
| `-l`, `--label` | `<LABEL>` | Required label (repeatable — all labels must be present) |
| `-s`, `--state` | `<STATE>` | State filter: open (default), closed, all |
| `--limit` | `<LIMIT>` | Max results (default 30) |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git issue close`

```sh
homeboy git issue close [OPTIONS] <COMPONENT_ID>
```

Close an existing issue with a typed reason

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `-n`, `--number` | `<NUMBER>` | Issue number |
| `-r`, `--reason` | `<REASON>` | Close reason: completed (default) or not-planned. Use `not-planned` to suppress re-filing by `homeboy runs findings reconcile` — the GitHub-native signal for "we have decided not to fix this." |
| `-c`, `--comment` | `<COMMENT>` | Optional closing comment (markdown). Posted before the state transition. Prefer --comment-file for long content |
| `--comment-file` | `<PATH>` | Read closing comment from a file ("-" for stdin) |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git issue edit`

```sh
homeboy git issue edit [OPTIONS] <COMPONENT_ID>
```

Edit an existing issue's title, body, or labels

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `-n`, `--number` | `<NUMBER>` | Issue number |
| `-t`, `--title` | `<TITLE>` | New title (optional) |
| `-b`, `--body` | `<BODY>` | New body (markdown). Prefer --body-file for long content |
| `--body-file` | `<PATH>` | Read body from a file ("-" for stdin) |
| `--add-label` | `<LABEL>` | Add labels (repeatable) |
| `--remove-label` | `<LABEL>` | Remove labels (repeatable) |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git pr`

```sh
homeboy git pr <COMMAND>
```

Manage GitHub pull requests for a component

| Subcommand | Summary |
| --- | --- |
| `homeboy git pr create` | Create a new pull request |
| `homeboy git pr edit` | Edit an existing PR's title or body |
| `homeboy git pr find` | Find PRs matching filters |
| `homeboy git pr readiness` | Explain PR merge readiness without attempting a merge |
| `homeboy git pr comment` | Post a comment on a PR. Three modes: |
| `homeboy git pr fleet` | Report and optionally land a fleet of pull requests |
| `homeboy git pr reconcile-mergeability` | Compare GitHub mergeability with local git merge-tree evidence |
| `homeboy git pr policy` | Evaluate PR open/merge policy |
| `homeboy git pr refresh` | Refresh a PR branch from its current base and report conflicts/checks |
| `homeboy git pr land` | Land a train of ready PRs sequentially, pausing on the first blocker |

## `homeboy git pr create`

```sh
homeboy git pr create [OPTIONS] <COMPONENT_ID>
```

Create a new pull request

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `-b`, `--base` | `<BASE>` | Base branch (target of the PR) |
| `-H`, `--head` | `<HEAD>` | Head branch (source of the PR) |
| `-t`, `--title` | `<TITLE>` | PR title |
| `-B`, `--body` | `<BODY>` | PR body (markdown). Prefer --body-file for long content |
| `--body-file` | `<PATH>` | Read body from a file ("-" for stdin) |
| `--draft` | flag | Open as draft |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git pr edit`

```sh
homeboy git pr edit [OPTIONS] <COMPONENT_ID>
```

Edit an existing PR's title or body

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `-n`, `--number` | `<NUMBER>` | PR number |
| `-t`, `--title` | `<TITLE>` | New title |
| `-B`, `--body` | `<BODY>` | New body (markdown) |
| `--body-file` | `<PATH>` | Read body from a file ("-" for stdin) |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git pr find`

```sh
homeboy git pr find [OPTIONS] <COMPONENT_ID>
```

Find PRs matching filters

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `-b`, `--base` | `<BASE>` | Base branch filter |
| `-H`, `--head` | `<HEAD>` | Head branch filter |
| `-s`, `--state` | `<STATE>` | State filter: open (default), closed, merged, all |
| `--limit` | `<LIMIT>` | Max results (default 30) |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git pr readiness`

```sh
homeboy git pr readiness [OPTIONS] <COMPONENT_ID>
```

Explain PR merge readiness without attempting a merge

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `-n`, `--number` | `<NUMBER>` | PR number |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git pr comment`

```sh
homeboy git pr comment [OPTIONS] <COMPONENT_ID>
```

Post a comment on a PR. Three modes:

1. Plain: no marker flags — a fresh comment is appended. 2. Sticky single-section (#1334): `--key <k>` finds-or-updates the one comment tagged `<!-- homeboy:key=<k> -->`. The whole `--body` becomes the comment body. 3. Sectioned (#1348): `--comment-key <outer> --section-key <inner>` merges `--body` into section `<inner>` of the shared comment tagged `<!-- homeboy:comment-key=<outer> -->`. Other sections are preserved. `--header` sets the line printed after the outer marker on new comments; `--footer` / `--footer-file` sets a block printed after the last section (e.g. a tooling-versions <details> block). Both are preserved from existing comments on merge when omitted. `--section-order` pins section ordering (CSV of keys); default is alphabetical.

Modes 2 and 3 are mutually exclusive. `--key` with `--comment-key` or `--section-key` is an error.

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `-n`, `--number` | `<NUMBER>` | PR number |
| `-B`, `--body` | `<BODY>` | Comment body (markdown). Prefer --body-file for long content |
| `--body-file` | `<PATH>` | Read body from a file ("-" for stdin) |
| `-k`, `--key` | `<KEY>` | Sticky whole-body key (mode 2, PR #1334). Mutually exclusive with --comment-key / --section-key |
| `--comment-key` | `<COMMENT_KEY>` | Sectioned mode: outer shared-comment key (mode 3, #1348). Must be combined with --section-key |
| `--section-key` | `<SECTION_KEY>` | Sectioned mode: inner per-section key (mode 3, #1348). Must be combined with --comment-key |
| `--header` | `<HEADER>` | Sectioned mode: optional header line written after the outer marker on freshly-created shared comments (e.g. "## Homeboy Results — `<component>`"). Existing comment headers are preserved on merge |
| `--footer` | `<FOOTER>` | Sectioned mode: optional footer block written after the last section (e.g. a tooling-versions <details> block). Existing footers are preserved on merge when this is omitted; passing --footer or --footer-file overwrites the preserved footer. Mutually exclusive with --footer-file |
| `--footer-file` | `<PATH>` | Sectioned mode: read footer content from a file ("-" for stdin). Mutually exclusive with --footer |
| `--section-order` | `<SECTION_ORDER>` | Sectioned mode: CSV of section keys in desired order. Sections listed here come first in the given order; others are appended alphabetically. Example: `--section-order lint,test,audit` |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git pr fleet`

```sh
homeboy git pr fleet [OPTIONS] <COMPONENT_ID> [PR]...
```

Report and optionally land a fleet of pull requests

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |
| `[PR]...` | no | PR numbers or URLs |

| Option | Value | Description |
| --- | --- | --- |
| `--update-branches` | flag | Update stale PR branches where GitHub can do so safely |
| `--apply` | flag | Merge green, clean PRs. Without this flag the command is read-only |
| `--merge-method` | `<MERGE_METHOD>` | Merge method: merge, squash, or rebase |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git pr reconcile-mergeability`

```sh
homeboy git pr reconcile-mergeability [OPTIONS] <COMPONENT_ID>
```

Compare GitHub mergeability with local git merge-tree evidence

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `-n`, `--number` | `<NUMBER>` | PR number |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git pr policy`

```sh
homeboy git pr policy <COMMAND>
```

Evaluate PR open/merge policy

| Subcommand | Summary |
| --- | --- |
| `homeboy git pr policy open` | Evaluate whether Homeboy may create or update a proposed PR |
| `homeboy git pr policy merge` | Evaluate whether an existing PR is safe to merge; optionally merge it |

## `homeboy git pr policy open`

```sh
homeboy git pr policy open [OPTIONS] <COMPONENT_ID>
```

Evaluate whether Homeboy may create or update a proposed PR

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `--policy` | `<PATH>` | Policy file path (YAML or JSON) |
| `--source` | `<SOURCE>` | Change source, e.g. autofix, deps, generated, release-prep, agent |
| `--base` | `<BASE>` | Base branch |
| `--head` | `<HEAD>` | Head branch |
| `--head-repo` | `<HEAD_REPOSITORY>` | Head repository owner/name |
| `--repository` | `<REPOSITORY>` | Base repository owner/name |
| `--file` | `<PATH>` | Changed file path. Repeatable |
| `--files-file` | `<PATH>` | Read changed file paths from a newline-delimited file |
| `--files-from-git` | flag | Read changed files from the current git working tree |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git pr policy merge`

```sh
homeboy git pr policy merge [OPTIONS] <COMPONENT_ID>
```

Evaluate whether an existing PR is safe to merge; optionally merge it

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |

| Option | Value | Description |
| --- | --- | --- |
| `--policy` | `<PATH>` | Policy file path (YAML or JSON) |
| `-n`, `--number` | `<NUMBER>` | PR number |
| `--author` | `<AUTHOR>` | Author login override. Defaults to GitHub PR metadata |
| `--base` | `<BASE>` | Base branch override. Defaults to GitHub PR metadata |
| `--head` | `<HEAD>` | Head branch override. Defaults to GitHub PR metadata |
| `--head-repo` | `<HEAD_REPOSITORY>` | Head repository owner/name override. Defaults to GitHub PR metadata |
| `--repository` | `<REPOSITORY>` | Base repository owner/name override. Defaults to component remote |
| `--merge` | flag | Merge the PR when policy allows it |
| `--merge-method` | `<MERGE_METHOD>` | Merge method: merge, squash, or rebase |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git pr refresh`

```sh
homeboy git pr refresh [OPTIONS] <COMPONENT_ID> <PR>
```

Refresh a PR branch from its current base and report conflicts/checks

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID |
| `<PR>` | yes | PR number or GitHub pull request URL |

| Option | Value | Description |
| --- | --- | --- |
| `--strategy` | `<STRATEGY>` | Update strategy. `auto` uses branch/pull rebase git config, falling back to rebase Values: `auto`, `rebase`, `merge`, `ff-only`. |
| `--push` | flag | Push the refreshed PR branch when the worktree is clean and checks pass. Uses --force-with-lease for rebase safety; plain force is not exposed |
| `--check` | `<COMMAND>` | Lightweight check command to run after a clean refresh. Repeatable. Defaults to `git diff --check` when omitted |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json |

## `homeboy git pr land`

```sh
homeboy git pr land [OPTIONS] <REPO> [PR]...
```

Land a train of ready PRs sequentially, pausing on the first blocker

| Argument | Required | Description |
| --- | --- | --- |
| `<REPO>` | yes | Repository as owner/repo or host/owner/repo |
| `[PR]...` | no | PR numbers or URLs. URLs must point at the selected repo |

| Option | Value | Description |
| --- | --- | --- |
| `--merge-method` | `<MERGE_METHOD>` | Merge method: merge, squash, or rebase Values: `merge`, `squash`, `rebase`. |
| `--delete-branch` | flag | Delete the PR branch after merge |
| `--dry-run` | flag | Inspect and report what would land without merging or refreshing |
| `--refresh-helper` | `<PROGRAM>` | Safe helper program used to refresh a dirty dependent PR. Not run through a shell. Combine with --refresh-helper-arg |
| `--refresh-helper-arg` | `<ARG>` | Argument for --refresh-helper. Supports {repo}, {number}, {url}, {head_sha} |
| `--max-base-retries` | `<MAX_BASE_RETRIES>` | Retry merge after this many base-branch-modified races |
| `--max-check-wait-seconds` | `<MAX_CHECK_WAIT_SECONDS>` | Maximum seconds to wait for all checks on the exact PR head to become terminal |
| `--check-waiver` | `<HEAD_SHA|CHECK_NAME|APPROVER>` | Waive one non-required failed check as HEAD_SHA\|CHECK_NAME\|APPROVER |
