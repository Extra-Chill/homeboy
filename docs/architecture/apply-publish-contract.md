# Apply And Publish Contract

Apply is a local mutation boundary: an adapter verifies a change artifact and
changes files in the target worktree. Committing, pushing, opening pull requests,
releasing, and deploying are separate workflow operations, not part of the
change-artifact wire payload.

## Core Wire Types

The runner/Lab contract lives in
`crates/homeboy-core/src/change_artifact.rs`:

- `ChangeArtifact` carries a source snapshot, a unified patch or file delta,
  optional provenance, and an optional digest. `ApplyChangeArtifact` is an alias
  for this same type.
- `ChangeApplyResult` reports apply status, snapshot hashes, modified files,
  optional artifact metadata, and any apply error. `ApplyChangeResult` is its
  adapter-facing alias.

These are the `homeboy/change-artifact/v1` and
`homeboy/change-apply-result/v1` wire schemas. There is no separate generic
execution, approval, or publish envelope; those policies and results belong to
the workflows that implement them.

## Apply Adapter Boundary

An apply adapter owns resolving the target worktree, verifying its input,
checking snapshot drift, enforcing path confinement, mutating files, and
reporting the result. Workflows remain responsible for their own approval and
publication policies.

`runner.workspace.apply` consumes patch and delta inputs locally. Source
snapshots provide drift detection, delta paths determine affected files, and
the result reports modified files without requiring a release-run projection.
