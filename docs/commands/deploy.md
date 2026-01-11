# `homeboy deploy`

## Synopsis

```sh
homeboy deploy <project_id> [<component_ids...>] [--all] [--outdated] [--build] [--dry-run]
```

## Arguments and flags

- `project_id`: project ID
- `<component_ids...>` (optional): component IDs to deploy (trailing var args)
- `--all`: deploy all configured components
- `--outdated`: deploy only components whose local and remote versions differ
- `--build`: run each component’s configured build command before deploying
- `--dry-run`: compute what would be deployed without uploading

## JSON output

```json
{
  "projectId": "<id>",
  "all": false,
  "outdated": false,
  "build": false,
  "dryRun": false,
  "components": [
    {
      "id": "<component id>",
      "name": "<name>",
      "status": "would_deploy|deployed|failed",
      "localVersion": "<v>|null",
      "remoteVersion": "<v>|null",
      "error": "<string>|null",
      "artifactPath": "<path>|null",
      "remotePath": "<path>|null",
      "buildCommand": "<cmd>|null",
      "buildExitCode": 0,
      "scpExitCode": 0
    }
  ],
  "summary": { "succeeded": 0, "failed": 0, "skipped": 0 }
}
```

## Exit code

- `0` when all selected component deploys succeed.
- `1` when any component deploy fails.

## Related

- [build](build.md)
- [component](component.md)
