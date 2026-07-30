<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy deploy` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/deploy.md](../../../commands/deploy.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy deploy`

```sh
homeboy deploy [OPTIONS] [TARGET_ID] [COMPONENT_IDS]...
```

Deploy components to remote server

| Argument | Required | Description |
| --- | --- | --- |
| `[TARGET_ID]` | no | Target ID: project ID or component ID (order is auto-detected) |
| `[COMPONENT_IDS]...` | no | Additional component IDs (enables project/component order detection) |

| Option | Value | Description |
| --- | --- | --- |
| `-p`, `--project` | `<PROJECT>` | Explicit project ID (takes precedence over positional detection) |
| `-c`, `--component` | `<COMPONENT>` | Explicit component IDs (takes precedence over positional) |
| `--json` | `<JSON>` | JSON input spec for bulk operations (array or {"component_ids": [...]}) |
| `--all` | flag | Deploy all configured components |
| `--outdated` | flag | Deploy only components whose local version differs from deployed remote |
| `--behind-upstream` | flag | Deploy only components whose local checkout is behind upstream |
| `--dry-run` | flag | Preview what would be deployed without executing |
| `--apply` | flag | Confirm dangerous deploy modes like --head, --ref, or --force |
| `--check` | flag | Check component status without building or deploying |
| `--force` | flag | Deploy even with uncommitted changes |
| `--projects` | `<PROJECTS>` | Deploy to multiple projects (comma-separated or repeated) |
| `-f`, `--fleet` | `<FLEET>` | Deploy to all projects in a fleet |
| `-s`, `--shared` | flag | Deploy to all projects using the specified component(s) |
| `--keep-deps` | flag | Keep build dependencies (skip post-deploy cleanup) |
| `--no-pull` | flag | Skip auto-pulling latest changes before deploy |
| `--allow-stale-source` | flag | Deploy a local build even when its source checkout is behind its upstream |
| `--allow-downgrade` | flag | Deploy a local build even when its semantic version is older than the remote |
| `--head` | flag | Deploy from current branch HEAD instead of the latest tag |
| `--release-set` | `<PATH>` | Validate this versioned release-set manifest before any deploy action |
| `--ref` | `<GIT_REF_OR_SHA>` | Deploy an exact Git ref resolved from the declared component repository |
| `--tagged` | flag | Force local tag-based build/deploy, ignoring reusable release assets |
| `--resume` | `<RUN_ID>` | Resume a prior multi-project deploy run after exact identity validation |
