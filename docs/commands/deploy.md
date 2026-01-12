# `homeboy deploy`

## Synopsis

```sh
homeboy deploy <projectId> [<componentIds...>] [--all] [--outdated] [--dry-run]
# If no component IDs are provided, you must use --all or --outdated.
```

## Arguments and flags

- `projectId`: project ID
- `<componentIds...>` (optional): component IDs to deploy (trailing var args)
- `--all`: deploy all configured components
- `--outdated`: deploy only outdated components (based on first configured version target)
- `--dry-run`: compute what would be deployed without executing

If no component IDs are provided and neither `--all` nor `--outdated` is set, Homeboy returns an error.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is `data.payload`.

```json
{
  "projectId": "<projectId>",
  "all": false,
  "outdated": false,
  "dryRun": false,
  "components": [
    {
      "id": "<componentId>",
      "name": "<name>",
      "status": "would_deploy|deployed|failed",
      "localVersion": "<v>|null",
      "remoteVersion": "<v>|null",
      "error": "<string>|null",
      "artifactPath": "<path>|null",
      "remotePath": "<path>|null",
      "buildCommand": "<cmd>|null",
      "buildExitCode": "<int>|null",
      "scpExitCode": "<int>|null"
    }
  ],
  "summary": { "succeeded": 0, "failed": 0, "skipped": 0 }
}
```

Note: `buildExitCode`/`scpExitCode` are numbers when present (not strings).

## Exit code

- `0` when all selected component deploys succeed.
- `1` when any component deploy fails.

## Related

- [build](build.md)
- [component](component.md)
