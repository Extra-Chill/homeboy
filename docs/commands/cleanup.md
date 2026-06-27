# cleanup

Remove or inspect reconstructable artifacts that Homeboy can safely recreate.

## `homeboy cleanup artifacts`

Scans the current repository and its managed Git worktrees for declared artifact paths. The command defaults to dry-run JSON output and only removes files when `--apply` is passed.

Built-in artifact names:

- `target` for Rust build output
- `node_modules` for Node dependencies
- `dist` for generated distribution output

Projects can add repo-relative paths with `artifact_cleanup_paths` in `homeboy.json`.

```bash
homeboy cleanup artifacts
homeboy cleanup artifacts --path /path/to/checkout
homeboy cleanup artifacts --apply
```

The JSON output includes worktree identity, candidate paths, estimated bytes, skipped reasons, applied rows, and a `summary` object. `summary.invocation_reclaimed_bytes` reports bytes reclaimed by the current command, `summary.remaining_candidate_bytes` reports cleanup candidates still present after the command, and `summary.cumulative_session_reclaimed_bytes` carries the local cumulative total for repeated `--apply` runs against the same repository. Cleanup refuses unsafe path declarations and skips artifact paths that contain tracked or staged source changes.
