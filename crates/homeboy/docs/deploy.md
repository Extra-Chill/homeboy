# deploy

Deploy configured components to a remote server.

```bash
homeboy deploy <project-id> [component-ids...] [flags]
```

## Options

- `--all` - Deploy all configured components for the project.
- `--outdated` - Only deploy components where the local version is newer than the remote version.
- `--build` - Run the component's `buildCommand` before deploying.
- `--dry-run` - Show what would be deployed without executing the deployment.
- `--json` - Output result as JSON.

## Requirements

The `deploy` command requires:
- A configured project with a linked server.
- The project must have a `basePath` set.
- Components must have `localPath`, `remotePath`, and `buildArtifact` configured.

Use `homeboy help deploy` for the authoritative flag list.
