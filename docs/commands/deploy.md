# `homeboy deploy`

## Synopsis

```sh
homeboy deploy [<project_id>|<component_id>] [<component_ids...>] [-p|--project <id>] [-c|--component <id>]... [--all] [--outdated|--behind-upstream] [--check] [--dry-run] [--json '<spec>']
# If no component IDs are provided, you must use --all, --outdated, --behind-upstream, or --check.

# Multi-project deployment
homeboy deploy --projects <project1>,<project2> <component_ids...>

# Fleet deployment
homeboy deploy <component_id> --fleet <fleet_id>

# Shared component deployment (auto-detect projects)
homeboy deploy <component_id> --shared
```

## Arguments and flags

- `project_id`: project ID
- `<component_ids...>` (optional): component IDs to deploy (positional, trailing)

Options:

- `-c`, `--component`: component ID to deploy (can be repeated, alternative to positional)
- `-p`, `--project`: explicit project ID; takes precedence over positional project/component detection
- `--all`: deploy all configured components
- `--outdated`: deploy only outdated components
  - Determined from the first version target for each component.
- `--behind-upstream`: deploy only components whose local checkout is behind upstream. Conflicts with `--outdated`.
- `--check`: check component status without building or deploying
  - Shows all components for the project with version comparison status.
  - Combines with `--outdated` or component IDs to filter results.
- `--dry-run`: preview what would be deployed without executing (no build, no upload)
- `--force`: deploy even with uncommitted changes
- `--json`: JSON input spec for bulk operations (`{"component_ids": ["component-id", ...]}`)
- `--projects`: deploy to multiple projects (comma-separated). When using this flag, all positional arguments are treated as component IDs. The build artifact is reused across projects.
- `-f`, `--fleet`: deploy to all projects in a fleet. Resolves fleet to project IDs, then runs multi-project deployment.
- `-s`, `--shared`: deploy to all projects using the specified component(s). Auto-detects which projects have the component configured and deploys to all of them.
- `--keep-deps`: keep build dependencies and skip post-deploy cleanup.
- `--version <version>`: assert the expected local component version before deploying; aborts on mismatch.
- `--no-pull`: skip the automatic pull before deploy.
- `--head`: deploy the current branch `HEAD` instead of the latest tag.
- `--tagged`: force tag-based deploy and ignore reusable build artifacts.

Bulk JSON input uses `component_ids` (snake_case):

```json
{ "component_ids": ["component-a", "component-b"] }
```

Positional and flag component IDs can be mixed; both are merged into the deployment list.

If no component IDs are provided and none of `--all`, `--outdated`, `--behind-upstream`, or `--check` is set, Homeboy returns an error. If `--outdated` or `--behind-upstream` finds no matching components, Homeboy returns an error.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below is `data`.

```json
{
  "command": "deploy.run",
  "project_id": "<project_id>",
  "all": false,
  "outdated": false,
  "behind_upstream": false,
  "check": false,
  "dry_run": false,
  "force": false,
  "results": [
    {
      "id": "<component_id>",
      "status": "deployed|failed|skipped|planned|checked",
      "deploy_reason": "explicitly_selected|all_selected|version_mismatch|unknown_local_version|unknown_remote_version",
      "component_status": "up_to_date|needs_update|behind_remote|behind_upstream|unknown",
      "local_version": "<v>|null",
      "remote_version": "<v>|null",
      "error": "<string>|null",
      "artifact_path": "<path>|null",
      "remote_path": "<path>|null",
      "build_exit_code": "<int>|null",
      "deploy_exit_code": "<int>|null",
      "release_state": {
        "commits_since_version": 5,
        "code_commits": 4,
        "docs_only_commits": 1,
        "has_uncommitted_changes": false,
        "baseline_ref": "v0.9.15"
      },
      "deployed_ref": "<tag-or-branch>|null"
    }
  ],
  "summary": { "total": 1, "succeeded": 0, "failed": 0, "skipped": 0 }
}
```

Notes:

- `deploy_reason` is omitted when not applicable.
- `component_status` is only present when using `--check` or `--check --dry-run`.
- `artifact_path` is the component build artifact path as configured; it may be relative but must include a filename.
- Deploy output does not include `build_command`. Builds are resolved from the linked extension, and deploy records only build/deploy exit codes plus the artifact path used.
- `deployed_ref` is omitted when no tag or branch ref was deployed.

Note: `build_exit_code`/`deploy_exit_code` are numbers when present (not strings).

### Component status values

When using `--check`, each component result includes a `component_status` field:

- `up_to_date`: local and remote versions match
- `needs_update`: local version ahead of remote (needs deployment)
- `behind_remote`: remote version ahead of local (local is behind)
- `behind_upstream`: local checkout is behind its upstream branch
- `unknown`: cannot determine status (missing version information)

### Release state

When using `--check`, each component result includes a `release_state` field that tracks unreleased changes:

- `commits_since_version`: number of commits since the last version tag
- `code_commits`: number of non-doc commits since the last version tag; omitted when zero
- `docs_only_commits`: number of docs-only commits since the last version tag; omitted when zero
- `has_uncommitted_changes`: whether there are uncommitted changes in the working directory
- `baseline_ref`: the tag or commit hash used as baseline for comparison

`baseline_ref`, `baseline_warning`, `code_commits`, and `docs_only_commits` are omitted when empty.

This helps identify components where `component_status` is `up_to_date` but work has been done since the current version baseline (`commits_since_version > 0`), indicating a release may be needed before deployment.

Exit code is `0` when `summary.failed == 0`, otherwise `1`.

## Exit code

- `0` when all selected component deploys succeed.
- `1` when any component deploy fails.

## Multi-Project Deployment

When a component belongs to multiple projects, use `--projects` to deploy to all of them in a single command:

```sh
# Deploy sample-plugin to both extra-chill and sarai-chinwag projects
homeboy deploy --projects extra-chill,sarai-chinwag sample-plugin

# Deploy multiple components to multiple projects
homeboy deploy --projects extra-chill,sarai-chinwag sample-plugin extrachill-api

# Preview multi-project deployment
homeboy deploy --projects extra-chill,sarai-chinwag sample-plugin --dry-run
```

The component is built once and the artifact is reused for all subsequent project deployments.

### Multi-project JSON output

When using `--projects`, the output structure differs:

```json
{
  "command": "deploy.run_multi",
  "component_ids": ["sample-plugin"],
  "dry_run": false,
  "check": false,
  "force": false,
  "projects": [
    {
      "project_id": "extra-chill",
      "status": "deployed|failed|planned|checked",
      "error": "<string>|null",
      "results": [...],
      "summary": { "total": 1, "succeeded": 1, "skipped": 0, "failed": 0 }
    },
    {
      "project_id": "sarai-chinwag",
      "status": "deployed|failed|planned|checked",
      "error": "<string>|null",
      "results": [...],
      "summary": { "total": 1, "succeeded": 1, "skipped": 0, "failed": 0 }
    }
  ],
  "summary": { "total_projects": 2, "succeeded": 2, "failed": 0, "skipped": 0, "planned": 0 }
}
```

Exit code is `1` if any project deployment fails.

## Fleet Deployment

Deploy to all projects in a named fleet:

```sh
# Deploy my-plugin to all projects in the production fleet
homeboy deploy my-plugin --fleet production

# Preview fleet deployment
homeboy deploy my-plugin --fleet production --dry-run

# Check status across fleet before deploying
homeboy fleet check production
```

See [fleet](fleet.md) for fleet management commands.

### Fleet vs Shared: When to Use Which

`--fleet` and `--shared` often produce the same result, especially in smaller setups where a fleet's projects are exactly the set of projects that use a given component. The difference is in **how they resolve targets**:

- **`--fleet <name>`** targets a **named group of projects**. The fleet is an explicit list you maintain. Use this when you want organizational control — e.g., deploying only to "production" projects, or deploying a component to a fleet even if not every project in it uses that component yet.
- **`--shared`** targets **every project that has the component configured**. It auto-detects from project configs. Use this when you want to update a component everywhere it's used, regardless of fleet membership.

**Rule of thumb:** Use `--shared` for "update this component everywhere." Use `--fleet` for "update this fleet specifically."

In practice, if your fleet membership mirrors your component usage, they're interchangeable — but as your fleet grows (staging vs production, multi-site networks), the distinction becomes meaningful.

## Shared Component Deployment

Deploy to all projects using a component, auto-detected:

```sh
# Deploy my-plugin to every project that uses it
homeboy deploy my-plugin --shared

# See which projects would be affected
homeboy component shared my-plugin

# Preview shared deployment
homeboy deploy my-plugin --shared --dry-run
```

This is useful when you don't have a named fleet but want to update a component everywhere it's used.

## Preview Before Deploying

Use `--dry-run` to see what would be deployed without executing:

```sh
homeboy deploy myproject --outdated --dry-run
homeboy deploy myproject --behind-upstream --dry-run
homeboy deploy myproject my-plugin --version 1.2.3 --tagged --dry-run
```

## Check Component Status

Use `--check` to view version status for all components without building or deploying:

```sh
# Check all components for a project
homeboy deploy myproject --check

# Check only outdated components
homeboy deploy myproject --check --outdated

# Check specific components
homeboy deploy myproject --check component-a component-b
```

To see detailed git changes (commits, diffs) before deploying, use the `changes` command:

```sh
# Show changes for all project components
homeboy changes --project myproject

# Show changes with git diffs included
homeboy changes --project myproject --git-diffs
```

## Post-Deploy Hooks

After a successful deploy, Homeboy runs `post:deploy` hooks remotely via SSH on the deployment target. Hooks are resolved from extensions and components (see [hooks](../architecture/hooks.md)).

Template variables available:

| Variable | Description |
|----------|-------------|
| `{{component_id}}` | The component ID |
| `{{install_dir}}` | Remote install directory |
| `{{base_path}}` | Project base path on the remote server |

### Extension-level hooks

Extensions like WordPress define `post:deploy` hooks in their manifest. These run for every component using that extension:

```json
{
  "hooks": {
    "post:deploy": [
      "wp plugin is-installed {{component_id}} --path={{base_path}} --allow-root 2>/dev/null && wp plugin activate {{component_id}} --path={{base_path}} --allow-root 2>/dev/null || true",
      "wp cache flush --path={{base_path}} --allow-root 2>/dev/null || true"
    ]
  }
}
```

### Component-level hooks

Components can add their own `post:deploy` hooks for custom automation:

```json
{
  "hooks": {
    "post:deploy": ["systemctl restart my-service"]
  }
}
```

Extension hooks run first, then component hooks. All `post:deploy` hooks are non-fatal — failures are logged but do not affect the deploy result.

## Related

- [build](build.md)
- [changes](changes.md)
- [component](component.md)
- [fleet](fleet.md)
- [hooks](../architecture/hooks.md)
