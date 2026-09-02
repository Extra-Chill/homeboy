<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
cargo run -p homeboy-cli --bin generate-cli-reference
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy release` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/release.md](../../../commands/release.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy release`

```sh
homeboy release [OPTIONS] [COMPONENTS]... [COMMAND]
```

Plan release workflows

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENTS]...` | no | Component ID(s) to release |

| Option | Value | Description |
| --- | --- | --- |
| `-p`, `--project` | `<PROJECT>` | Release all components in a project that need a release |
| `--outdated` | flag | Only release components with unreleased code commits (use with --project) |
| `--path` | `<PATH>` | Override local_path for version file lookup (single component only) |
| `--preflight-runner` | `<RUNNER_ID>` | Run portable lint and test release gates through the existing Lab review commands before controller-owned release mutation |
| `--preflight-placement` | `<PREFLIGHT_PLACEMENT>` | Placement policy for portable release preflight gates Values: `local`, `lab`. |
| `--dry-run` | flag | _no help text_ |
| `--full` | flag | Emit the complete release command-result envelope on stdout |
| `--apply` | flag | Confirm risky release execution modes |
| `--deploy` | flag | Deploy to all projects using this component after release |
| `--recover` | flag | Recover from an interrupted release (tag + push current version) |
| `--owner-run-ref` | `<OWNER_RUN_REF>` | Provider workspace owner reference to reconcile during --recover |
| `--retag` | flag | With --recover: move a stale tag to HEAD, or recreate an unpublished divergent tagged release on the current branch. Divergent recovery also requires --bump <version> to name the tag Homeboy may replace |
| `--head` | flag | Finish the release pipeline for an already-versioned, already-tagged HEAD. Skips changelog/version/git mutation steps and runs package, GitHub Release, publish, cleanup, and post-release hooks against the tag pointing at HEAD |
| `--from-artifacts` | `<DIR>` | Use existing release artifacts from this directory instead of running release.package. Requires --head |
| `--package-only` | flag | Regenerate only the release package for an existing tag at HEAD. Combine with --head --tag <tag> --apply to write a durable artifact inventory for later --head --from-artifacts finalization |
| `--tag` | `<TAG>` | Existing release tag to package with --package-only |
| `--skip-checks` | `<CHECK>` | Skip pre-release quality checks |
| `--skip-build-validation` | flag | Bypass the package/build-structure validation while still running the build |
| `--bump` | `<BUMP>` | Force a specific version bump: major, minor, patch, or an explicit version (e.g. 2.0.0). Overrides auto-detection from commit history |
| `--force-lower-bump` | flag | Allow an explicit bump lower than Homeboy's commit-derived recommendation |
| `--skip-publish` | flag | Skip registry/package publishing only (version bump + tag + push). This does NOT skip GitHub Release creation — a GitHub Release is still created unless you ALSO pass --no-github-release. Use when CI handles registry/package publishing after the tag is pushed |
| `--no-github-release` | flag | Skip the GitHub Release creation step (the reviewer-facing release page with notes + assets on github.com). The tag is still created and pushed |
| `--i-know-ci-creates-the-github-release` | flag | Confirm that --no-github-release is intentional on a manual/local release because CI (or another pipeline) creates the GitHub Release. Required whenever --no-github-release is used on a component that would otherwise get a reviewer-facing GitHub Release |
| `--i-know-this-is-a-manual-tag-only-release` | flag | Confirm an intentional manual tag-only release. Use only when no CI-owned GitHub Release automation should create the reviewer-facing release page |
| `--git-identity` | `<GIT_IDENTITY>` | Git identity for release commits and tags. Use "bot" for the default CI bot identity, or "Name <email>" for custom. When set, configures git user.name and user.email before committing |
| `--cascade` | flag | After releasing the component, release every dependent that declares a dependency on it: update the dependent's declared dependency pin and release it with an automatic patch bump, transitively. Single-component releases only |

| Subcommand | Summary |
| --- | --- |
| `homeboy release changes` | Show changes since the last version tag |
| `homeboy release changelog` | Show generated changelog content |
| `homeboy release version` | Version inspection helpers |
| `homeboy release artifact-source-authority` | Write a source-authority manifest for assembled release artifacts |
| `homeboy release contains` | Report which release first contained a commit, and whether the installed build has it |
| `homeboy release gap` | Report how far the installed build is behind the newest release |
| `homeboy release readiness` | Inspect retained portable release-readiness evidence |

## `homeboy release changes`

```sh
homeboy release changes [OPTIONS] [TARGET_ID] [COMPONENT_IDS]...
```

Show changes since the last version tag

| Argument | Required | Description |
| --- | --- | --- |
| `[TARGET_ID]` | no | Target ID: component ID (single mode) or project ID (if followed by component IDs) |
| `[COMPONENT_IDS]...` | no | Component IDs to filter (when target_id is a project) |

| Option | Value | Description |
| --- | --- | --- |
| `--project` | `<PROJECT>` | Show changes for all components in a project (alternative to positional project mode) |
| `--path` | `<PATH>` | Workspace path to operate on directly. Useful for unregistered checkouts (CI runners, ad-hoc clones, worktrees) |
| `--json` | `<JSON>` | JSON input spec for bulk operations: {"componentIds": ["id1", "id2"]} |
| `--since` | `<SINCE>` | Compare against specific tag instead of latest |
| `--git-diffs` | flag | Include commit range diff in output (uncommitted diff is always included) |

## `homeboy release changelog`

```sh
homeboy release changelog [COMMAND]
```

Show generated changelog content

| Subcommand | Summary |
| --- | --- |
| `homeboy release changelog show` | Show Homeboy's changelog, or a component changelog when an ID is provided |

## `homeboy release changelog show`

```sh
homeboy release changelog show [COMPONENT_ID]
```

Show Homeboy's changelog, or a component changelog when an ID is provided

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID]` | no | Component ID to show changelog for |

## `homeboy release version`

```sh
homeboy release version <COMMAND>
```

Version inspection helpers

| Subcommand | Summary |
| --- | --- |
| `homeboy release version show` | Show current version (default: discovered component, fallback: homeboy binary) |

## `homeboy release version show`

```sh
homeboy release version show [OPTIONS] [COMPONENT_ID]
```

Show current version (default: discovered component, fallback: homeboy binary)

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID]` | no | Component ID (optional - shows discovered component version, or homeboy binary version) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override local_path for version file lookup |

## `homeboy release artifact-source-authority`

```sh
homeboy release artifact-source-authority [OPTIONS] <COMPONENT_ID>
```

Write a source-authority manifest for assembled release artifacts

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component prepared for finalization |

| Option | Value | Description |
| --- | --- | --- |
| `--dir` | `<DIR>` | Directory containing the assembled publication files |
| `--tag` | `<TAG>` | Prepared release tag |
| `--commit` | `<COMMIT>` | Exact commit the prepared tag resolves to |
| `--release-notes` | `<PATH>` | Exact persisted GitHub Release body to bind as a non-publication control artifact |

## `homeboy release contains`

```sh
homeboy release contains [OPTIONS] [COMPONENT_ID_OR_COMMIT] [COMMIT]
```

Report which release first contained a commit, and whether the installed build has it

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID_OR_COMMIT]` | no | Component ID when followed by COMMIT or paired with --issue. A lone value remains the commit-ish for compatibility |
| `[COMMIT]` | no | Commit sha (or any commit-ish) to locate after COMPONENT_ID |

| Option | Value | Description |
| --- | --- | --- |
| `--issue` | `<N>` | Resolve the commit through the merged pull request that closed this issue, so the operator does not have to find the sha first |
| `--component` | `<COMPONENT_ID>` | Component whose release tag namespace to search (default: the component discovered from the working directory) |
| `--path` | `<PATH>` | Checkout to inspect directly. Useful for unregistered clones, CI runners, and worktrees |
| `--installed` | `<VERSION>` | Version to treat as installed instead of the component checkout's version |

## `homeboy release gap`

```sh
homeboy release gap [OPTIONS] [COMPONENT_ID]
```

Report how far the installed build is behind the newest release

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID]` | no | Component whose release tag namespace to search (default: the component discovered from the working directory) |

| Option | Value | Description |
| --- | --- | --- |
| `--component` | `<COMPONENT_ID>` | Component whose release tag namespace to search (alternative to the positional component target) |
| `--path` | `<PATH>` | Checkout to inspect directly. Useful for unregistered clones, CI runners, and worktrees |
| `--installed` | `<VERSION>` | Version to treat as installed instead of the component checkout's version |

## `homeboy release readiness`

```sh
homeboy release readiness <COMMAND>
```

Inspect retained portable release-readiness evidence

| Subcommand | Summary |
| --- | --- |
| `homeboy release readiness show` | Show one retained readiness operation by operation:// reference or ID |
| `homeboy release readiness list` | List retained readiness operations for a component |

## `homeboy release readiness show`

```sh
homeboy release readiness show <REFERENCE>
```

Show one retained readiness operation by operation:// reference or ID

| Argument | Required | Description |
| --- | --- | --- |
| `<REFERENCE>` | yes | Readiness operation:// reference or operation ID |

## `homeboy release readiness list`

```sh
homeboy release readiness list <COMPONENT_ID>
```

List retained readiness operations for a component

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component whose retained readiness operations to list |
