# Direct Staged Execution

Tracker: [#14588](https://github.com/Extra-Chill/homeboy/issues/14588).
This follows the preventive capability refusal in
[#14589](https://github.com/Extra-Chill/homeboy/pull/14589).

## Contract

Direct sessions negotiate `direct-runner-staged-execution/v1` before submitting
the unchanged sealed staging request to `POST /runner/staging/direct`. A daemon
without that capability remains refused. Reverse transport continues to use
`POST /runner/staging` and its existing worker-claim lifecycle.

The direct handler persists or replays the original staging receipt, then selects
that exact queued job for direct execution. The selection is durable and excludes
reverse claims before source preparation. Direct replay preparation is serialized
under the existing staging lock. Atomic adoption preserves the job UUID,
submission-key index, event history, and source provenance while transferring
execution into the normal daemon-local capacity and child lifecycle.

The runner driver shares the reverse worker's source verification. It retains a
directory descriptor during preparation and capacity waiting, rechecks ownership
before execution, and uses that descriptor as the child working directory. Public
environment overrides, declared secret names, and the durable run identity are
carried into the execution envelope; secret values remain runner-resolved.

Terminal and concurrent receipt replays do not launch a second command or restore
source files over a running command's mutations. The original runner-job cancel
endpoint remains valid after adoption and uses the local child's cancellation
and reap path. Nonzero exit status, stdout, and stderr remain result evidence on
the original job.

## Interrupted Adoption

A new daemon lease alone does not authorize takeover. Startup captures the exact
previous `PidDead` lease verdict under the daemon-owner lock, excludes conflicting
foreground owners, and limits recovery to adopted queued jobs without a child
reservation. The shared driver prepares the retained sealed source, and recovery
transfers the exact original job to the new worker under the proven-dead lease.
Old workers are fenced at child reservation. The generation registry transfers
the original job route and counts without moving unrelated jobs or double-counting
replays. Preparation failure becomes terminal diagnostic evidence for the normal
retry lifecycle instead of another apparently accepted queue entry.

This is same-store recovery and receipt replay. It does not forcibly replace a
live draining daemon, migrate arbitrary stores between generations, cancel live
jobs, or authorize a runtime rollout.

## Verification

The managed Lab run `homeboy-14588-final-gates-v2` passed workspace compilation,
format checks, and all nine selected test groups (121 test executions):

```sh
cargo fmt --all --check
cargo check --workspace
cargo test -p homeboy-core --lib staged -- --test-threads=1
cargo test -p homeboy-core --lib new_daemon_lease_recovers_adopted_pre_worker_job_once -- --test-threads=1
cargo test -p homeboy-core --lib daemon::generation_store::tests -- --test-threads=1
cargo test -p homeboy-core --lib remote_runner -- --test-threads=1
cargo test -p homeboy-lab-runner --lib execution::tests::daemon_exec:: -- --test-threads=1
cargo test -p homeboy-lab-runner --lib runner_staging_store::tests:: -- --test-threads=1
cargo test -p homeboy-lab-runner --lib runner_staging_operation:: -- --test-threads=1
cargo test -p homeboy-lab-runner --lib reverse_worker_executes_a_verified_staged_source_package -- --test-threads=1
cargo test -p homeboy-lab-runner --lib retained_staged_workspace_authority_refuses_path_replacement_before_spawn -- --test-threads=1
```

The daemon-execution tests call the production routes and real runner driver to
execute sealed-file reads, preserve public environment, observe one side effect
under concurrent replay, retain a nonzero child result, and cancel/reap a child.
Core tests cover direct-versus-reverse selection, durable recovery state,
generation routing, and explicit preparation failure. These are focused gates,
not a claim that the entire repository test suite passed.

The existing submitted-authority test failed on immutable base
`68d9f780d039bcdc59bf5bf556a7aec4c173d482` because its malformed binding failed
deserialization before reaching the intended policy. Its fixture is now a
well-formed but unauthorized binding; the policy refusal and no-owner-registration
assertions remain intact. Baseline run: `homeboy-14588-baseline-authority-guard`.

Operator-retained final evidence:

- Runner job: `82f21707-9c2d-4229-9fe0-457b1d0810e8`
- Artifact: `runner-exec-fd16faee5d370b15fe6c5e14ded9e356aa1b2f6100062bbb4ca87d5a48af78ed`
- File: `homeboy-14588-verification.log`
- SHA-256: `3aa1d911b9d6d0b52fba1edc3b8ad77a3a1c5cecf7408935906853d6f53117f7`
- Candidate base: `68d9f780d039bcdc59bf5bf556a7aec4c173d482`, plus this change

These IDs resolve in the operator's Homeboy store, not a public artifact host.
The commands above are the repository-native reproduction path. The original
MDI consumer parity workload has not yet completed through an installed version
of this repair; live runtime convergence and retained-generation recovery remain
separate operational steps.
