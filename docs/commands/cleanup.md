# cleanup

Remove or inspect reconstructable artifacts that Homeboy can safely recreate.

This is the canonical artifact cleanup path. Worktree lifecycle cleanup is handled by `homeboy worktree cleanup`; artifact removal stays dry-run here until `--apply` is passed.

## `homeboy cleanup artifacts`

Scans the current repository and its managed Git worktrees for built-in and declared artifact paths. The command defaults to dry-run output and only removes files when `--apply` is passed.

Homeboy always treats Rust `target` directories as rebuildable artifacts. Projects can add repo-relative cleanup paths with `artifact_cleanup_paths` in `homeboy.json`.

```bash
homeboy cleanup artifacts
homeboy cleanup artifacts --path /path/to/checkout
homeboy cleanup artifacts --sort size --limit 10
homeboy cleanup artifacts --merged-only --sort size --limit 10
homeboy cleanup artifacts --apply
```

Use `--sort size` to review the largest artifacts first, `--limit N` to bound the reported or removed candidates after sorting, and `--merged-only` to preserve artifacts from worktrees whose branch is not merged into its upstream.

The JSON output includes worktree identity, candidate paths, estimated bytes, skipped reasons, applied rows, and a `summary` object. The terminal summary shows bounded candidate rows and points to the JSON output for full large reviews. `summary.invocation_reclaimed_bytes` reports bytes reclaimed by the current command, `summary.remaining_candidate_bytes` reports cleanup candidates still present after the command, and `summary.cumulative_session_reclaimed_bytes` carries the local cumulative total for repeated `--apply` runs against the same repository. Cleanup refuses unsafe path declarations and skips artifact paths that contain tracked or staged source changes.

Regular status/cleanup disk-pressure integration is tracked separately from this command surface; use `homeboy cleanup artifacts --sort size --limit 10` as the explicit review path until that integration lands.

## Shared Cargo Targets

Homeboy-managed Cargo builds use shared stores below its local data directory. Inspect them through the normal cleanup inventory:

```bash
homeboy cleanup --include shared-cargo-targets
homeboy cleanup --include shared-cargo-targets --apply
```

`retention.shared_store_days` defaults to `30`, `retention.shared_store_max_bytes` defaults to `21474836480` (20 GiB), and `retention.shared_store_lease_seconds` defaults to `21600` (6 hours). The age and size budgets select rebuildable stores; the lease window independently protects active workloads. Inventory output is bounded by `retention.limit`; when `next_command` is present, run it to continue from `next_cursor`.
