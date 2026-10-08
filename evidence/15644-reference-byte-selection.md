# #15644 reference-to-byte selection handoff

Base: `4d83d29b1938fe946eb08f5baa7ecaba357aa789`.
Branch: `fix/15644-reference-byte-selection`. Changes are uncommitted.

## Ownership

`AgentTaskArtifactSelector` in the existing artifact-reference contract owns the
strict producer-qualified URI subset. Status projection reuses it, including
selector preservation and unknown-fragment removal.

`runs_service::select_artifact_record` owns selection for local retrieval,
connected-runner retrieval, and the existing friendly HTTP content reader.
Exact run-scoped byte IDs precede reference lookup and friendly aliases. Typed
`ControlPlaneRef` artifact references and non-colliding bare status pointer IDs
resolve through the existing run-scoped reference owner. A recorded friendly
kind/name/original-manifest ID remains a friendly token even when it has the
synthetic-ID shape; the typed form disambiguates a colliding pointer. Shape alone
never establishes pointer authority. Logical selection uses persisted
`agent_task.task_id` and `agent_task.logical_artifact_id` metadata, without deriving
observation IDs or duplicating controller authority ranks. Multiple records fail
with canonical candidate IDs, even when their recorded digests agree.

The connected-runner adapter requests an exhaustive inventory, refuses explicitly
incomplete pages, and feeds the selected canonical ID into the existing download
path. Exact daemon byte routes retain their existing implementation.

## Commands and results

All cargo invocations were offloaded using:

```sh
homeboy runner exec --sync-workspace "$WORKTREE" \
  --workspace-sync-timeout 900s --run-id "$RUN_ID" homeboy-lab -- \
  env CARGO_TARGET_DIR="$TARGET" RUST_MIN_STACK=16777216 CARGO_BUILD_JOBS=2 \
  <command>
```

`WORKTREE` was the permitted outer workspace's
`.tmp-test/homeboy-orchestration/fix-15644-bytes`; baseline checks used
`.tmp-test/homeboy-orchestration/baseline-15644-bytes`, a detached worktree at the
same base with only test additions. Earlier checks added the canonical-pin
regression; the final same-surface check replaced that test-only diff with only
the status-pointer CLI regression described below.

### Baseline behavioral regression

- Run: `bytes-15644-baseline-pin-02`.
- Job: `c9961971-6956-4981-81bf-01fe7837ac2b`.
- Target: `/home/chubes/.local/share/homeboy/cargo-targets/component-01912fb78693`.
- Command: `cargo test -p homeboy-cli artifact_get_canonical_pin_outranks_older_friendly_alias -- --nocapture`.
- Result: exit 101, 0 passed / 1 failed. Retrieval returned `older-alias` instead
  of the exact canonical ID. The candidate passes this same regression.
- An earlier attempt, `bytes-15644-baseline-pin-01`, failed to compile because the
  new test initially named a nonexistent store method. The corrected test uses
  the existing `import_artifact` API; that initial failure is not behavioral proof.

### Initial isolated candidate gates (before transport follow-up)

- Run: `bytes-15644-isolated-green-07`.
- Job: `9791b106-13d0-4550-a330-32849fc24fe5`.
- Exclusive target: `/home/chubes/.local/share/homeboy/cargo-targets/fix-15644-reference-byte-selection`.
- Source snapshot: `sha256:fd30216eba3c4904e4825c3451d636f0c807e7f10eb3c435ed400899d42ee099`.
- Command:

```sh
sh -c 'cargo test -p homeboy-core reference_byte_selection -- --nocapture && cargo test -p homeboy-core artifact_content_serves -- --nocapture && cargo test -p homeboy-core canonical_run_scoped_route_does_not_resolve_friendly_aliases -- --nocapture && cargo test -p homeboy-cli artifact_get_ -- --nocapture && cargo test -p homeboy-cli runner_reference_byte_selection -- --nocapture && cargo test -p homeboy-agents terminal_executor_artifacts_are_projected_under_logical_ids -- --nocapture && cargo test -p homeboy-artifact-ref-contract -- --nocapture'
```

Authoritative job result: succeeded, exit 0. Counts respectively: 2, 2, 1, 6, 1,
1, 6 passed (19 tests total); no failures. The installed exec bridge subsequently
reported missing `agent_task_run` metadata. Actual results were checked with:

```sh
homeboy runner job logs homeboy-lab 9791b106-13d0-4550-a330-32849fc24fe5 --compact --tail 8
```

`cargo fmt --all -- --check` and `git diff --check` passed locally.

### Real HTTP transport follow-up and final current gates

Follow-up source adds
`crates/homeboy-cli/src/commands/runs/remote_transport_test.rs`, included by
`remote.rs`, and corrects bare-token collision handling in the shared selector.
The server is the existing `daemon::serve_listener_until_shutdown` fixture API;
its owner wakes the listener and joins its helpers on both success and unwinding.
The fixture persists an `AgentTaskRunRecord` through `AgentTaskLifecycleStore`,
uses the production orchestration provider, and records actual retained files in
the ObservationStore. It does not construct any HTTP response envelope.

Transport assertions establish:

1. A real status GET yields the actual synthetic pointer.
2. `/runs/<run>/artifacts?full=1` yields the real exhaustive inventory with at least
   61 records, with the target beyond the default 50-record page.
3. `/v1/control-plane/runs/<run>/artifacts/<reference>` yields the actual daemon
   response shape: outer `success/data`, then `HttpApiResponse.body`, then the
   versioned `ControlPlaneResult<ControlPlaneReference>.resource`. The schema,
   requested run, and producer-qualified URI are asserted.
4. Both bare and typed pointer tokens pass those actual HTTP responses through
   the runner acquisition selector and produce `controller-retained-patch`, a
   real record ID independent of hashing conventions.
5. The existing `daemon::fetch_artifact_to_path` performs the actual canonical
   `/runs/<run>/artifacts/controller-retained-patch/content` HTTP download after
   the original producer file has been removed. Destination bytes and independently
   computed SHA-256 match the retained record; size and response digest agree.
6. Actual direct byte requests using the pointer, friendly `patch` token, raw path,
   or file URI return HTTP 404. Raw-path runner acquisition also fails. Exact
   direct routes have not become reference/friendly resolvers.

First focused verification:

- Run `bytes-15644-http-proof-08`, job `fbf9bbda-0378-47f5-80f9-9e7d392b9422`.
- Exclusive target `/home/chubes/.local/share/homeboy/cargo-targets/fix-15644-reference-byte-selection`.
- Sequential commands:
  `cargo test -p homeboy-core reference_byte_selection -- --nocapture --test-threads=1`
  then
  `cargo test -p homeboy-cli runner_reference_byte_selection_downloads_through_real_daemon_http_after_producer_removal -- --nocapture --test-threads=1`.
- Exit 0: 3 selection tests and 1 real HTTP transport test passed.

Final current-source verification:

- Run `bytes-15644-transport-green-09`, job `acdebd89-dfdc-4b52-bd70-a9ba2044d8fb`.
- Same exclusive target, `RUST_MIN_STACK=16777216`, `CARGO_BUILD_JOBS=2`.
- Command, using the runner exec wrapper above:

```sh
sh -c 'cargo test -p homeboy-core reference_byte_selection -- --nocapture --test-threads=1 && cargo test -p homeboy-core artifact_content_serves -- --nocapture --test-threads=1 && cargo test -p homeboy-core canonical_run_scoped_route_does_not_resolve_friendly_aliases -- --nocapture --test-threads=1 && cargo test -p homeboy-cli artifact_get_ -- --nocapture --test-threads=1 && cargo test -p homeboy-cli runner_reference_byte_selection -- --nocapture --test-threads=1 && cargo test -p homeboy-agents terminal_executor_artifacts_are_projected_under_logical_ids -- --nocapture --test-threads=1 && cargo test -p homeboy-artifact-ref-contract -- --nocapture --test-threads=1'
```

Runner exec returned authoritative job success and exit 0, without a bridge error.
Counts respectively: 3, 2, 1, 6, 2, 1, 6 passed (**21 tests**, no failures).
Local format and diff checks passed. No production source changed after this run;
only this evidence handoff was updated.

The bare-token regression covers all three friendly fields (`kind`, `name`, and
`original_manifest_id`) with an `artifact-<32hex>` token. Friendly retrieval does
not depend on reference availability; the explicit typed token performs real
reference selection. A separate assertion preserves exact canonical-pin priority
and another requires an actual successful lookup when no friendly token exists.

### Same CLI surface: actual status pointer -> artifact_get, baseline and candidate

Added `artifact_get_status_pointer_retrieves_retained_patch_after_producer_removal`
to `crates/homeboy-cli/src/commands/runs/tests/mod.rs` on both checkouts. The test
body is identical and uses only APIs available at `4d83d29b1`:

- `AgentTaskLifecycleStore::write_record` persists the real lifecycle record.
- The registered production orchestration owner supplies the actual status
  resource through `core::control_plane::run`, including the synthetic pointer.
- `ObservationStore::record_artifact_with_id` retains the actual patch and
  producer-qualified metadata under `controller-retained-cli-patch`.
- The fixture records the producer's real SHA-256 before removing its file.
- It calls the **existing CLI `artifact_get` handler**, first with the bare status
  pointer and then with its typed artifact form. It never calls the new
  `select_artifact_record` primitive directly.
- On success, it asserts the handler's canonical result ID, destination bytes,
  and independently recomputed destination digest against the original digest.

Before baseline verification, `git diff --stat` showed only the test file,
76 insertions; `git status --short` showed only that file, and HEAD remained
`4d83d29b1`. The earlier canonical-pin test was removed from this baseline fixture
so the baseline diff contains only this newly requested behavioral regression.
Production baseline source was unchanged.

Executed sequentially, with separate exclusive targets:

```sh
homeboy runner exec --sync-workspace "/Users/chubes/Developer/data-liberation-agent@roadie-fork-66f5be7a-91c5-4352-b09c-73270d132935/.tmp-test/homeboy-orchestration/baseline-15644-bytes" --workspace-sync-timeout 900s --run-id bytes-15644-status-baseline-10 homeboy-lab -- env CARGO_TARGET_DIR=/home/chubes/.local/share/homeboy/cargo-targets/fix-15644-reference-byte-selection-baseline RUST_MIN_STACK=16777216 CARGO_BUILD_JOBS=2 cargo test -p homeboy-cli artifact_get_status_pointer_retrieves_retained_patch_after_producer_removal -- --nocapture --test-threads=1
```

- Run: `bytes-15644-status-baseline-10`.
- Job: `0a7c50e8-7cb4-4cc2-ad5f-b449d4a8dfd4`.
- Result: **exit 101, 0 passed / 1 failed**. Compilation and fixture setup passed.
  The test failed at the first, bare-pointer `artifact_get` call:
  `artifact record not found: artifact-ead203f92dfed088381afe6e13b3f699`.
  The diagnostic listed the retained record:
  `patch, controller-retained-cli-patch`. The original producer had already been
  removed. This is the status-reference lookup defect, not canonical-pin precedence.
  The typed iteration was not reached on baseline.

```sh
homeboy runner exec --sync-workspace "/Users/chubes/Developer/data-liberation-agent@roadie-fork-66f5be7a-91c5-4352-b09c-73270d132935/.tmp-test/homeboy-orchestration/fix-15644-bytes" --workspace-sync-timeout 900s --run-id bytes-15644-status-candidate-11 homeboy-lab -- env CARGO_TARGET_DIR=/home/chubes/.local/share/homeboy/cargo-targets/fix-15644-reference-byte-selection RUST_MIN_STACK=16777216 CARGO_BUILD_JOBS=2 cargo test -p homeboy-cli artifact_get_status_pointer_retrieves_retained_patch_after_producer_removal -- --nocapture --test-threads=1
```

- Run: `bytes-15644-status-candidate-11`.
- Job: `b16b10c6-29fa-457a-8e7a-c4718fcc07a1`.
- Result: **exit 0, 1 passed / 0 failed**. Both bare and typed pointer iterations
  retrieved the retained patch through `artifact_get`, with independent destination
  bytes/digest assertions passing after producer removal.
- No production changes were needed for this follow-up. Only the regression and
  this evidence handoff were added. Format and diff checks passed locally.

This same-surface regression complements the previously passing 21-test slice
and accepted real loopback HTTP proof; it does not claim SSH, reverse-broker,
installed runner-session, or whole-CLI-subprocess `--runner` coverage.

### Additional checks and environmental failures

- `bytes-15644-final-gates-03`, job `b7246e9f-83b0-49c7-9ed3-71a0b704c554`:
  authoritative exit 0. The broader `observation::runs_service::` selection ran
  112 passing tests, alongside exact-daemon, CLI, lifecycle, and existing
  status-selector tests. This preceded the self-review correction limiting bare
  pointer detection to the actual status-ID shape, preserving `artifact-report`
  friendly names. Final isolated selection tests cover that correction.
- `bytes-15644-review-gates-04`, job `88caa0c9-6b2f-43db-a7b0-226c18ffcfda`:
  selection tests passed; broad `cargo test -p homeboy-core http_api:: -- --nocapture`
  returned 56 passed / 3 failed. Job/cancel fixtures reported unavailable SQLite
  paths or `InternalUnexpected` where validation was expected. The `&&` chain
  stopped there; subsequent commands in that invocation did not run.
- Baseline repeat `bytes-15644-baseline-http-05`, job
  `296a6160-a076-4176-ac13-5e80654ebfd3`: the same broad HTTP command returned
  53 passed / 6 failed. `test_handle_with_jobs` and
  `strict_projection_cancel_requires_exact_durable_local_runner_binding` reproduced
  the unavailable SQLite errors. Other failed fixtures varied. Artifact-content
  tests passed on both versions. No unrelated fixture or production guard was changed.
- `bytes-15644-scoped-green-06`, job `76ea2afd-4401-4fba-930c-6bb4ed9a4285`:
  shared-target compile failed with an unresolved selector import despite that
  type being present in the synced source. The prior shared target was contended;
  an exclusive task-specific cold target resolved the problem without source edits.

## Real-byte proof and boundaries

The lifecycle test submits and records a successful aggregate containing a real
patch with actual SHA-256 and size, projects it into a real ObservationStore,
obtains its status pointer, and asks `OrchestrationService::reference` for the
run-scoped metadata. Both bare status ID and typed artifact reference select the
actual retained record. The original producer file is removed before retrieval;
the existing copier writes the retained bytes, and assertions independently hash
the destination and compare the expected bytes.

CLI tests exercise the existing handler and copier against real retained store
records after producer removal, canonical precedence over an older conflicting
alias, and rejection of filesystem/file-URI and wrong-task inputs. The remote
adapter acquisition test uses real retained records and a supplied daemon-metadata
response, puts the target beyond the default 50-record page, and rejects a
truncated response. The follow-up test above adds actual daemon HTTP metadata and
an actual byte download, rather than treating that supplied response/local copier
test as transport proof.

Missing records, wrong run/task, malformed/duplicate selectors, metadata-only
records, missing local bytes, and multiple/conflicting logical projections fail
visibly. Unsupported reference URI forms fail with canonical-ID guidance rather
than treating a path as byte authorization. The local copier still reports the
recorded digest; the proof's independent hashing does not imply universal runtime
integrity checking was added.

## Review considerations

- Friendly alias ambiguity now fails instead of picking the oldest record.
- Generic selection intentionally does not collapse equivalent controller copies;
  callers can pin one of the listed real canonical IDs.
- Connected-runner selection uses the existing exhaustive inventory API, so its
  metadata lookup cost grows with the run inventory.
- Bare friendly tokens retain their legacy meaning even when they look like
  synthetic IDs. A colliding pointer must be requested as `artifact/<reference-id>`.
- Auto-selection is limited to the supported producer-qualified agent-task URI;
  other URI forms provide explicit canonical-ID guidance and remain unproven.
- The new transport proof is an actual loopback direct-daemon HTTP test through
  the runner metadata selection helper and the existing direct HTTP downloader.
  It does not exercise an SSH tunnel, reverse broker, installed runner session
  routing, or a whole CLI subprocess invocation with `--runner`.
- Broad HTTP fixtures remain a baseline-reproduced blocker to an all-green HTTP
  suite; the repaired byte-selection slice is green on an exclusive target.

Recommended PR title: **fix(runs): resolve status artifact references before byte retrieval**.
Summary: distinguish control-plane pointer identity from retained byte identity;
share producer-qualified/canonical selection across readers; preserve exact daemon
routes; prove retrieval after producer cleanup with independent destination hashes.
