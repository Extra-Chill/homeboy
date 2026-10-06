# Scoped snapshot seed lookup — #15531

## Root cause

Baseline: `a1e68babd`, freshly built from main. A two-file source fixture did not
finish `runner exec --sync-workspace` within 300 seconds. A separate invocation
with `--workspace-sync-timeout 30s` was sampled while running. Its live stack was:

```text
exec_request -> workspace_context -> sync_workspace_before
-> compatible_incremental_snapshot -> workspace_snapshots_for_runner
-> workspace_snapshots_ssh -> SshClient::execute -> child.wait
```

Incremental seed selection requested every historical snapshot before filtering
the source path and policy. Its unbounded SSH child bypassed invocation control.

## Repair

Replace that hot-path full inventory with source-directory-family candidates,
using the canonical snapshot-directory writer's basename normalization. Preserve
the exact path, exclusion-policy and content-manifest checks before selecting the
newest compatible seed. Run the lookup through existing `WorkspaceControl`
subprocess ownership, deadline and cancellation. Full operator inventory remains
a separate supported operation; there is no inventory fallback on the hot path.

## Deterministic verification

```sh
CARGO_BUILD_JOBS=2 cargo test -p homeboy-lab-runner --lib source_lookup_tests -- --test-threads=1
# 3 passed: source scope/order, deadline, cancellation after process start
CARGO_BUILD_JOBS=2 cargo test -p homeboy-lab-runner --lib workspace::tests::snapshots -- --test-threads=1
# 20 passed: incremental deltas, zero transfer, corrupt seed, changed policy,
# another source path, prepared caches, listing and publication races
cargo fmt --all -- --check
git diff --check
```

The existing core `replaced_relative_to` dead-code warning remains. No full-suite
claim is made.

## Real runner evidence

The repaired canonical `runner workspace sync` materialized the two-file fixture
on the populated Lab. Its authoritative receipt reported:

- 708 final bytes, two files;
- 178 bytes/one file reused from the prior source snapshot;
- 530 bytes/one file transferred;
- source snapshot identity `snapshot:bf1cb87d5d2c8df8`;
- prepared workspace lease `workspace:e092dbfa-41d7-411e-b42c-461491934116`.

The restored service then admitted the command against that exact materialized
workspace. Persisted run `homeboy-15531-materialized-execution-proof-20261005`,
runner job `06cb5437-a4d1-4be7-96a1-3219ba176adc`, exited zero and verified Linux
execution and source marker SHA-256:
`6a12e808557102b0ff9783d0db16c84a624d3e7f87092df23de0ea100cf8fa66`.

The controller-scoped service had separately been absent; its supported install
and connect operations restored real daemon execution. Controller binary upgrade
remains separately fenced by an older unresolved orchestration projection. This
evidence uses the repaired materializer and the restored installed daemon; it
does not claim a matched-runtime Cook-to-PR rollout or completed binary upgrade.

## Provenance

OpenAI `gpt-6.1-sol`, OpenCode, directly investigated and implemented this repair
in an isolated tracker-linked worktree under operator authorization. Tests ran
locally; source materialization and execution proof used the real Lab. Finalization
is outside Homeboy coding orchestration after the recorded managed blocker.
