# cleanup

Remove or inspect reconstructable artifacts that Homeboy can safely recreate.

## `homeboy cleanup artifacts`

Scans the current repository and its managed Git worktrees for declared artifact paths. The command defaults to dry-run JSON output and only removes files when `--apply` is passed.

Built-in artifact paths:

- `target/` for Rust build output
- `node_modules/` for Node dependencies
- `dist/` for generated distribution output

Projects can add repo-relative paths with `artifact_cleanup_paths` in `homeboy.json`.

```bash
homeboy cleanup artifacts
homeboy cleanup artifacts --path /path/to/checkout
homeboy cleanup artifacts --apply
```

The JSON output includes worktree identity, candidate paths, estimated bytes, skipped reasons, and applied rows. Cleanup refuses unsafe path declarations and skips artifact paths that contain tracked or staged source changes.
