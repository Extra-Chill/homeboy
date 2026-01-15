# `homeboy deploy`

## Synopsis

```sh
homeboy deploy <projectId> [<componentIds...>] [-c|--component <id>]... [--all] [--outdated] [--dry-run] [--json '<spec>']
# If no component IDs are provided, you must use --all or --outdated.
```

## Arguments and flags

- `projectId`: project ID
- `<componentIds...>` (optional): component IDs to deploy (positional, trailing)

Options:

- `-c`, `--component`: component ID to deploy (can be repeated, alternative to positional)
- `--all`: deploy all configured components
- `--outdated`: deploy only outdated components
  - Determined from the first version target for each component.
- `--dry-run`: preview what would be deployed without executing (no build, no upload)
- `--json`: JSON input spec for bulk operations (`{"component_ids": ["component-id", ...]}`)

Positional and flag component IDs can be mixed; both are merged into the deployment list.

If no component IDs are provided and neither `--all` nor `--outdated` is set, Homeboy returns an error.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is `data`.

```json
{
  "command": "deploy.run",
  "project_id": "<projectId>",
  "all": false,
  "outdated": false,
  "dry_run": false,
  "results": [
    {
      "id": "<componentId>",
      "name": "<name>",
      "status": "deployed|failed|skipped|planned",
      "deploy_reason": "explicitly_selected|all_selected|version_mismatch|unknown_local_version|unknown_remote_version",
      "local_version": "<v>|null",
      "remote_version": "<v>|null",
      "error": "<string>|null",
      "artifact_path": "<path>|null",
      "remote_path": "<path>|null",
      "build_command": "<cmd>|null",
      "build_exit_code": "<int>|null",
      "deploy_exit_code": "<int>|null"
    }
  ],
  "summary": { "succeeded": 0, "failed": 0, "skipped": 0 }
}
```

Notes:

- `deploy_reason` is omitted when not applicable.
- `artifact_path` is the component build artifact path as configured; it may be relative.

Note: `build_exit_code`/`deploy_exit_code` are numbers when present (not strings).

Exit code is `0` when `summary.failed == 0`, otherwise `1`.

## Exit code

- `0` when all selected component deploys succeed.
- `1` when any component deploy fails.

## Preview Before Deploying

Use `--dry-run` to see what would be deployed without executing:

```sh
homeboy deploy myproject --outdated --dry-run
```

To see detailed git changes (commits, diffs) before deploying, use the `changes` command:

```sh
# Show changes for all project components
homeboy changes --project myproject

# Show changes with git diffs included
homeboy changes --project myproject --git-diffs
```

## Related

- [build](build.md)
- [changes](changes.md)
- [component](component.md)
