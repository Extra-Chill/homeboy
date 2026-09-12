# Validation Dependency Recovery

Tracking: [#14587](https://github.com/Extra-Chill/homeboy/issues/14587).
Related execution limitation: [#14588](https://github.com/Extra-Chill/homeboy/issues/14588).
Evidence reviewed September 12, 2026.

## Changes

An explicit `validation_dependencies` setting now replaces the manifest's list
for hygiene and Lab source selection, including an explicit empty array. Values
must be arrays of nonempty strings; malformed overrides fail rather than
silently disabling dependency validation. Without an override, manifest
selection is unchanged.

CLI settings-file, string-setting, and JSON-setting precedence is resolved
before Lab materialization. The profile snapshot is shared with portable
argument construction. The selected sources flow through workspace sync and
controller-to-runner path remapping. Internal selection metadata is removed
before constructing the remote command.

Native worktree reuse now checks active registration, cleanliness, containment,
and linked Git/branch identity independently from destructive cleanup safety.
A clean committed task branch is reusable even when its commits are unpushed.
Cleanup still refuses unpushed work, and promotion's existing authorized dirty
candidate path is preserved. This does not change historical records' base refs
or correct inflated cleanup counts caused by stale local base branches.

The controller also refuses sealed staging for direct-SSH sessions, whose
daemon currently has no consumer for that queue. Reverse-worker staging is
unchanged. This is a preventive capability correction only: it does not execute
or recover previously queued direct jobs. The v1 request wire shape is unchanged.

## Repository-Native Verification

Run from this candidate checkout. Tests use isolated fixture repositories;
single-threaded execution matches the repository's test setting.

```sh
cargo fmt --all --check
cargo check --workspace
cargo test -p homeboy-core --lib hygiene -- --test-threads=1
cargo test -p homeboy-core --lib worktree::tests:: -- --test-threads=1
cargo test -p homeboy-core --lib worktree_provider::tests:: -- --test-threads=1
cargo test -p homeboy-lab-runner --lib validation_dependency -- --test-threads=1
cargo test -p homeboy-lab-runner --lib planner_stages_and_strips_the_controller_selected_dependency -- --test-threads=1
cargo test -p homeboy-lab-runner --lib runner_staging_store::tests:: -- --test-threads=1
cargo test -p homeboy-cli --lib effective_json_override_applies_profile_string_and_json_precedence -- --test-threads=1
```

The final managed Lab run passed all seven test selections: 18, 64, 7, 11, 1,
9, and 1 tests respectively (111 total), plus workspace compilation and format
checks. The tests exercise selected clean versus dirty Git dependencies,
intentional empty selection, malformed input, source-relative resolution,
staging metadata, worktree identity and cleanup protection, and direct refusal
versus authenticated reverse staging.

Operator-retained evidence:

- Run: `homeboy-14587-final-gates`
- Runner job: `22b0627d-508c-4151-88fb-9ee804966fcc`
- Artifact: `runner-exec-d36987e21ab0b25b1dd16877a02049458644fddeb75a15aaa1e70691c8d45514`
- Artifact filename: `homeboy-14587-verification.log`
- Candidate base: `086a34929c8d914ce047f750d6b9d4969c59605e`, plus this change

These IDs resolve in the operator's Homeboy store, not a public artifact host.
The commands above are the reviewer-facing reproduction path.

## Live Replay Boundary

The original paired-consumer command was retried with the candidate controller.
It stopped before source staging with `selected runner requires admitted
connected readiness evidence`. Runner status identified controller version
`0.373.1` versus configured runner job binary `0.372.3`; a protected queued job
also prevented implicit daemon replacement. The existing installation and
queued job were left intact.

Consequently, the original WordPress consumer suite has not completed through
this candidate. Runtime convergence and safe recovery of the queued direct job
remain necessary before claiming end-to-end workload completion. Neither this
change nor its unit/integration gates establish SQL parity or authorize a
release, daemon replacement, or production migration.
