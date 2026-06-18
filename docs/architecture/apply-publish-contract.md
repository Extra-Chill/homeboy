# Apply And Publish Contract

Homeboy uses a shared execution vocabulary for generated changes:

1. `execute` produces proposed results.
2. `artifact` preserves proposed changes with provenance and digest metadata.
3. `approve` records the exact artifact, run, step, or file scope allowed to change.
4. `apply` materializes approved changes in a local worktree.
5. `publish` commits, pushes, opens a pull request, releases, or deploys.

`apply` is a local mutation boundary. An apply adapter verifies an approved
`ChangeArtifact`, checks safety policy, and changes files in the target worktree.
It does not commit, push, open a PR, release, or deploy. Those operations belong
to the publish layer, represented by `PublishRequest` and `PublishResult`.

## Core Types

The core contract lives in `src/core/execution.rs`:

- `ExecutionPhase` names the canonical phase vocabulary.
- `ChangeArtifact` stores proposed changes with provenance.
- `ApprovalScope` records what is approved.
- `ApplyRequest` and `ApplyResult` describe local worktree mutation.
- `ApplyAdapterContract` advertises supported artifact types and preflight policy.
- `ApplyPreflightFailure` reports shared safety failures.
- `PublishRequest` and `PublishResult` describe post-apply externalization.

## Apply Adapter Boundary

An apply adapter owns:

- resolving the target local worktree;
- verifying the artifact payload and provenance;
- validating approval coverage;
- checking snapshot drift when the artifact carries snapshot metadata;
- enforcing path confinement;
- mutating files in the local worktree;
- reporting changed files and preflight failures.

Publish owns:

- staging or committing the applied change;
- pushing branches or tags;
- opening or updating pull requests;
- creating releases;
- deploying artifacts.

## Shared Preflight Checks

Adapters should express failures with `ApplyPreflightCheck` values:

- `clean_worktree` for uncommitted or untracked local changes when the adapter
  requires a clean target.
- `protected_branch` for direct apply attempts on protected branch names such as
  `main`, `master`, or `trunk`.
- `approval_coverage` when the approval scope does not cover every file or
  artifact being applied.
- `snapshot_drift` when the current worktree no longer matches the artifact's
  captured source snapshot.
- `path_confinement` when an artifact path escapes the target worktree.
- `staged_file_expectation` when the final staged/changed file set does not
  match what the artifact declared.

## Lab Artifact Projection

`runner.workspace.apply` already applies Lab patch and delta inputs locally. Its
current JSON input can project into the shared contract without changing behavior:

- unified patches use artifact type `lab.patch.unified_diff`;
- deltas use artifact type `lab.delta.files`;
- `source_snapshot` metadata drives `snapshot_drift` checks;
- delta file paths drive `path_confinement` checks;
- `RunnerWorkspaceApplyOutput.modified_files` maps to `ApplyResult.files_changed`.

This issue only defines the shared contract. Existing Lab CLI behavior can keep
its current input and output shape while adapters migrate.

## Sample Runtime Migration Path

The existing `homeboy/sample-runtime-apply-adapter/v1` extension adapter can migrate
to the core contract in two steps:

1. Return or accept `ApplyAdapterContract` with artifact types such as
   `wp_sample-runtime.bundle` and `wp_sample-runtime.file`, plus an `ApplyPreflightPolicy`.
2. Map its current verify/apply/stage/commit/push/PR flow so verify and file
   mutation return `ApplyResult`, while commit, push, and PR creation move to
   `PublishRequest`/`PublishResult` or compatibility CLI flags that call publish
   after apply.

During migration, compatibility flags may preserve existing behavior, but the
canonical contract remains apply first and publish second.
