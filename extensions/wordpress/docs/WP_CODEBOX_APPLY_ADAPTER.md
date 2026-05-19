# WP Codebox Apply Adapter

The WordPress extension provides `homeboy/wp-codebox-apply-adapter/v1`, a parent-side adapter core for approved WP Codebox artifacts.

The adapter consumes the same approved payload shape passed by WP Codebox's `wp_codebox_apply_approved_artifact` filter, or a fixture artifact bundle directory with:

- `manifest.json`
- `metadata.json`
- `files/changed-files.json`
- `files/patch.diff`
- `files/review.json`

It re-computes the content digest from the exact bytes of `files/changed-files.json` and `files/patch.diff`, verifies the patch SHA-256, requires approval for every changed file in the current slice, applies that canonical patch to a clean git worktree, commits, and returns structured metadata.

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

## PR Seam

The core supports `--push` and `--open-pr`, but callers can also use the returned metadata and run the final PR step explicitly:

```bash
git push -u origin <branch>
gh pr create --fill
```

Production callers should wire bot identity, DMC worktree creation/reuse, branch naming, PR title/body policy, and AI assistance disclosure before enabling automatic PR creation.
