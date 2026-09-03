# WP Codebox Apply Adapter

The WordPress extension provides `homeboy/wp-codebox-apply-adapter/v1`, a parent-side adapter core for approved WP Codebox artifacts. It projects WP Codebox artifact bundles into the Homeboy core execution contract:

- `ChangeArtifact` for the reviewed WP Codebox patch bundle.
- `ApplyRequest` for the approved apply operation and its extension-local policy.
- `ApplyResult` for the adapter result.

The adapter consumes the same approved payload shape passed by WP Codebox's `wp_codebox_apply_approved_artifact` filter, or a fixture artifact bundle directory with:

- `manifest.json`
- `metadata.json`
- `files/changed-files.json`
- `files/patch.diff`
- `files/review.json`

It verifies the WP Codebox-approved payload, requires approval for every changed file in the current slice, and stages that canonical patch in a clean git worktree. Homeboy owns commit, push, and pull-request finalization.

WP Codebox-specific verification stays extension-local in `policy` and `metadata.wp_codebox`: content digest, patch digest, approved files, bundle path, review evidence, and changed-file metadata. The adapter also preserves clean-worktree, protected-branch, and path-confinement checks before applying.

```bash
node scripts/agent/wp-codebox-apply-adapter.cjs \
  --bundle /path/to/artifact-bundle \
  --worktree /path/to/safe/worktree \
  --approved-file /wordpress/wp-content/plugins/example/changed-file.php \
  --patch-strip 5
```

The command prints JSON containing `adapter_id`, `artifact_id`, `patch_sha256`, `content_digest`, `approved_files`, `applied_files`, `worktree`, and `branch`.

The command can also consume a core `ApplyRequest` JSON file:

```bash
node scripts/agent/wp-codebox-apply-adapter.cjs \
  --request /path/to/apply-request.json \
  --worktree /path/to/safe/worktree
```

`ApplyResult` fields are emitted at the top level:

- `id`
- `request_id`
- `status`
- `applied`
- `files_changed`
- `artifacts`
- `warnings`
- `error`
- `metadata`

Legacy fields such as `success`, `artifact_id`, and `applied_files` remain as compatibility output and are mirrored under `metadata.legacy`.

## Apply Boundary

The adapter validates and stages the approved patch. `metadata.apply_phase.staged` records that extension-local phase. Homeboy owns the subsequent commit, push, and pull-request lifecycle.
