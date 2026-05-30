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

It re-computes the content digest from the exact bytes of `files/changed-files.json` and `files/patch.diff`, verifies the patch SHA-256, requires approval for every changed file in the current slice, applies that canonical patch to a clean git worktree, commits, and returns an `ApplyResult`.

WP Codebox-specific verification stays extension-local in `policy` and `metadata.wp_codebox`: content digest, patch digest, approved files, bundle path, review evidence, and changed-file metadata. The adapter also preserves clean-worktree, protected-branch, and path-confinement checks before applying.

```bash
node scripts/agent/wp-codebox-apply-adapter.cjs \
  --bundle /path/to/artifact-bundle \
  --worktree /path/to/safe/worktree \
  --branch feature/apply-approved-artifact \
  --approved-file /wordpress/wp-content/plugins/example/changed-file.php \
  --patch-strip 5 \
  --commit-message "Apply approved WP Codebox artifact"
```

The command prints JSON containing `adapter_id`, `artifact_id`, `patch_sha256`, `content_digest`, `approved_files`, `applied_files`, `worktree`, `branch`, `commit`, and `pr_url` when a PR is created.

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

Legacy fields such as `success`, `artifact_id`, `applied_files`, `commit`, and `pr_url` remain as compatibility output and are mirrored under `metadata.legacy`.

## Apply And Publish Phases

Apply and publish are distinct phases:

- **Apply** validates the approved artifact, applies the patch to the clean worktree, and commits it locally. The local commit is reported in `metadata.apply_phase`.
- **Publish** is compatibility behavior for existing callers that pass `--push` or `--open-pr`. Push and PR state is reported in `metadata.publish_phase` and is not treated as part of the core apply contract.

## PR Seam

The adapter still supports `--push` and `--open-pr` for compatibility, but callers should prefer using the returned `ApplyResult.metadata.apply_phase` and running the final publish step explicitly:

```bash
git push -u origin <branch>
gh pr create --fill
```

Production callers should wire bot identity, DMC worktree creation/reuse, branch naming, PR title/body policy, and AI assistance disclosure before enabling automatic PR creation.
