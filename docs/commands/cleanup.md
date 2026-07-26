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
homeboy cleanup artifacts --min-age-days 7
homeboy cleanup artifacts --apply
```

Use `--sort size` to review the largest artifacts first, `--limit N` to bound the reported or removed candidates after sorting, and `--merged-only` to preserve artifacts from worktrees whose branch is not merged into its upstream.

The JSON output includes worktree identity, candidate paths, estimated bytes, skipped reasons, applied rows, a per-worktree `worktrees` roll-up, and a `summary` object. The terminal summary shows bounded candidate rows and points to the JSON output for full large reviews. `summary.invocation_reclaimed_bytes` reports bytes reclaimed by the current command, `summary.remaining_candidate_bytes` reports cleanup candidates still present after the command, and `summary.cumulative_session_reclaimed_bytes` carries the local cumulative total for repeated `--apply` runs against the same repository. Cleanup refuses unsafe path declarations and skips artifact paths that contain tracked or staged source changes, or untracked work that Git does not ignore.

Every candidate reports both `size_bytes` (apparent content size) and `allocated_bytes` (disk actually charged to the tree). Large trees of small files diverge sharply between the two; `allocated_bytes` is what free space reflects after `--apply`.

### Extension-declared artifacts

Installed extensions declare the reconstructable install/build trees they own through `artifact_cleanup` in their manifest. Homeboy resolves those declarations against every managed worktree and applies the same safety policy it applies to built-in paths — nothing about a toolchain is hardcoded in Homeboy.

- Declarations resolve only beside install scopes the extension supports. A scope is a directory carrying the manifest files the declaration names, so an artifact path is a candidate only where that ecosystem actually installs. Nested discovery is depth-bounded and never descends into a declared artifact tree.
- Candidates report `declared_by` (the owning extension), `category`, `readiness`, `liveness`, and `rehydrate_command`. The per-worktree roll-up lists the deduplicated commands that restore each checkout.
- Declarations in category `release_asset` are inventoried and never removed, so packaged output needed for deployment stays distinguishable from development output.
- Checkouts registered as active task worktrees are protected from extension-declared removal by default, because losing an install tree leaves a live checkout unusable until it is rehydrated. Pass `--include-active-worktrees` to reclaim them anyway. Built-in and `homeboy.json` declarations keep their existing behavior.
- `--min-age-days N` requires an artifact to be untouched for N days. A declaration can set its own floor; the stricter of the two applies.

Configured worktree-provider previews run independently with a 30-second cancellation boundary. The output records each provider's typed `outcome`, `inventory_completeness`, elapsed time, and heartbeat count, so a timed-out or failed provider yields a partial inventory without blocking healthy providers. Preview commands remain the only provider operation invoked without `--apply`.

Regular `homeboy cleanup` includes `repo-artifacts` inventory. After an agent-task provider exits, Homeboy also cleans declared rebuildable artifacts from that exact detached attempt worktree before applying its existing source-state and commit safety guard. Active sibling worktrees are not scanned by this lifecycle step.

## Shared Cargo Targets

Homeboy-managed Cargo builds use shared stores below its local data directory. Inspect them through the normal cleanup inventory:

```bash
homeboy cleanup --include shared-cargo-targets
homeboy cleanup --include shared-cargo-targets --apply
```

`retention.shared_store_days` defaults to `30`, `retention.shared_store_max_bytes` defaults to `21474836480` (20 GiB), and `retention.shared_store_lease_seconds` defaults to `21600` (6 hours). The age and size budgets select rebuildable stores; the lease window independently protects active workloads. Inventory output is bounded by `retention.limit`; when `next_command` is present, run it to continue from `next_cursor`.
