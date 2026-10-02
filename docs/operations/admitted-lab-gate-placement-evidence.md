# #15359 admitted Lab gate placement — editing-session evidence

Status: **source transport checklist completed with full native execution**;
supervisor acceptance/publication remains separate. The continuation exercised
the complete controller-to-runner path against owned candidate runtimes, not only
the admitted entry point. No publication or PR finalization was performed.

## Final supervisor source-review corrections — superseding boundary

The final reviewed source is verified by **full-transport-16**, not merely the
previous passing transport-15. Transport-15 and every failed/intermediate run
remain preserved as historical evidence below and in Homeboy's native records.

### Full closure and disposition corrections

- `seal_extension_resources` now retains the existing canonical
  `AgentTaskGateExtensionInputProvenance` alongside transport input mappings.
  Both terminal runner evidence and end-of-run controller re-materialization are
  compared against that sealed original: vector lengths, extension IDs/content
  identities/private destinations, and shared paths/identities/destinations.
  Source paths alone may differ for host materialization. No new closure hash
  vocabulary, store, queue or lifecycle was introduced.
- Controller source normalization now uses the admitted original provenance
  **after** complete revalidation; it does not rediscover live source paths and
  associate changed controller trees with old runner evidence.
- The real SSH heartbeat test changes **only** controller
  `shared/declared.txt` during a sleeping gate. Extension bytes and directory
  membership remain unchanged; the shared hash changes; the result is retained
  as failed at `controller_closure_revalidation`, with a custodied diagnostic.
  Fixture shared data is restored before subsequent checks.
- Promotion candidate binding distinguishes `admitted-lab-gate-receipt/v1` from
  `lab-gate-disposition/v1`. Admitted receipts must match the canonical checkout
  tree. Recognized `executed=false` non-passing dispositions retain unavailable/
  cancelled/failed outcome and remediation rather than a fabricated candidate
  mismatch. A passing result cannot use a disposition to bypass binding.
- Tagged unavailable Lab readiness is retained as a gate report by the promotion
  owner. Ordered gates stop on failed, unavailable or deferred prerequisites;
  successors are explicitly skipped. `Unavailable` remains blocked/deferred in
  the existing outcome bridge and is ineligible for finalization.

### Exact final full native command and evidence

```sh
homeboy runner exec \
  --run-id fix-15359-full-transport-16 \
  --sync-workspace /Users/chubes/Developer/homeboy@fix-15359-admitted-gate-placement \
  --env CARGO_TARGET_DIR=/home/chubes/Developer/_lab_workspaces/homeboy-fix-15359-admitted-gate-placement-a9af4c4cf18e-a84ecbd0-0e2b-4537-950e-d9732dae9c19-1790953844561112000/target \
  --env HOMEBOY_PRODUCT_GIT_COMMIT=73d50cb21df95861133c1f236e31d10c203fce28 \
  --env HOMEBOY_PRODUCT_GIT_DIRTY=true \
  --artifact full-transport-evidence homeboy-lab -- timeout 2400 bash -c \
  'cargo build --quiet -p homeboy && python3 scripts/verify-lab-gate-transport.py "$CARGO_TARGET_DIR/debug/homeboy" full-transport-evidence /home/chubes/.config/homeboy/extensions/rust'
```

- Run: `fix-15359-full-transport-16`.
- Native Lab job: `2c1d9ec9-7e21-4fae-a9b3-ea11519a3e9d`.
- Actual private SSH integration: **1 passed, 0 failed, 0 ignored**, 2220
  filtered; **350.46 seconds**.
- Actual installed Rust extension: **2 passed, 0 failed**, valid inventory.
- All transport-15 checks remain passing, plus shared-only controller mutation
  during execution is rejected. The native observer delivered **9** bounded
  heartbeat callbacks in the ordinary execution controls.
- Artifact: `dd9a3b54-a3b4-4e47-83aa-d368ae5eea53`, `full-transport-evidence`;
  SHA-256 `49112706093fa8c31ac8314bac833c460ad7f1f0c38cc57f813c3f009b458274`.
- `shared-mutation-source-identities.json` records identical extension before/
  after hash `2b95532169975c434453da626625cc4cd868a9d2074393b23b8ba47f8cb78aed`
  and distinct shared hashes. `shared-mutation-during-transport.json` retains the
  failed stage and diagnostic artifact reference. This is actual native mutation
  evidence, not a mocked receipt claimed as execution.

### Final native promotion/source/adapter checks

The final retained owner run uses the immutable source snapshot also verified by
the full run (same source materialization fingerprint `b425acbff442`):

```sh
homeboy runner exec --run-id fix-15359-final-review-owner-checks-final \
  --cwd /home/chubes/Developer/_lab_workspaces/homeboy-fix-15359-admitted-gate-placement-b425acbff442-7f817d28-4a04-49b6-bdd6-e45f0ee9a576-1790977555635715000 \
  --env CARGO_TARGET_DIR=/home/chubes/Developer/_lab_workspaces/homeboy-fix-15359-admitted-gate-placement-a9af4c4cf18e-a84ecbd0-0e2b-4537-950e-d9732dae9c19-1790953844561112000/target \
  --env HOMEBOY_PRODUCT_GIT_COMMIT=73d50cb21df95861133c1f236e31d10c203fce28 \
  --env HOMEBOY_PRODUCT_GIT_DIRTY=true \
  --artifact owner-verification-evidence homeboy-lab -- timeout 2400 \
  python3 scripts/verify-gate-owner-tests.py owner-verification-evidence
```

Run `fix-15359-final-review-owner-checks-final`, job
`69763d45-b5e7-4672-a6ef-b241cf8fb178`, artifact
`ce6ca0d9-258b-4bf9-9237-192850acf769`, SHA-256
`a45203ea8a630752230546857564bc83dd033dbbb08b45bc5d623ff0866f215d`.
The artifact records every exact expanded Cargo command, stdout, stderr and exit.

| Owner coverage | Counts |
| --- | --- |
| Gate protocol/full canonical closure comparison | 3 passed |
| CLI placement parser | 1 passed |
| Real Git promotion checkout/unavailable retention/ordered stop and corrupted executed-proof rejection | 2 passed |
| External installed source/writes with different ambient config: all three special trees and scripts/lib | 1 passed |
| Real successor runtime-generation publication, immutable predecessor and stable runtime link | 1 passed |
| Existing private-at-file owner | 7 passed |
| Compact unleased producer summary, running/reachable ambiguous ownership and retained work | 5 passed |
| Broader gate-owner baseline | **78 passed / 1 failed** |

All **20 focused tests pass**. The wrapper intentionally exits 1 for the unchanged
broader Rust-cache ownership/permissions failure; it does not waive #15328 or
report that suite as green. The corrupted-receipt consumer test is deliberately
an integrity-boundary rejection test, not synthetic full transport proof.

Additional actual adapter finding: compact status puts retained work outside the
full `/freshness` object. The existing count owner now preserves both shown and
omitted compact work and uses the maximum with full freshness counts, refusing
cold startup over retained work even without lease coordinates.

`fix-15359-final-review-owner-checks-1` retains the initial regression findings.
`fix-15359-final-review-owner-checks-2` ran all focused tests successfully and the
same 78/79 baseline, but its initiating CLI result projection failed with
`observation run ... is missing agent_task_run metadata`. Actual producer counts
and terminal exit 1 remain in native job `94d68771-45e0-481b-b564-cb66675765d1`
events (result #21359). The final retained run above succeeded in evidence custody
and truthfully returned test exit 1. This projection failure is recorded as an
independent control-plane/evidence issue, not disguised as passing transport.

Final whole-session scope: **25 Rust source paths** including existing-source
fixture/wiring updates and native verification, three verifier scripts and this
evidence note. This review's production delta is canonical closure comparison,
receipt/disposition binding/ordered retention, and compact retained-work counting;
the additional source-generation and adapter changes are regression coverage.
Shared source paths remain for supervisor merge review. No commits, public GitHub
disclosures, release, runtime-pin changes, operator-state changes or finalization
were performed. Supervisor still owns source acceptance and untouched published-
runtime proof after release.

## Supervisor-review continuation: transport-15 proof (preserved historical boundary)

Run `fix-15359-full-transport-15`, native Lab job
`8e994f37-b5c4-4799-9788-c0405f252e1f`, succeeded. The private fixture uses real
OpenSSH authentication with generated fixture-only keys and a loopback SSH
daemon. The controller and receiver use Lab-built candidate processes; the
receiver binary, HOME, config/data roots, native daemon, workspaces, SSH client
configuration and known-hosts file are fixture-owned. Operator registry,
credentials, shared jobs and selected runtime/config pins are not copied or
changed. Fixture daemons and SSH processes are stopped by their existing owners.

```sh
homeboy runner exec \
  --run-id fix-15359-full-transport-15 \
  --sync-workspace /Users/chubes/Developer/homeboy@fix-15359-admitted-gate-placement \
  --env CARGO_TARGET_DIR=/home/chubes/Developer/_lab_workspaces/homeboy-fix-15359-admitted-gate-placement-a9af4c4cf18e-a84ecbd0-0e2b-4537-950e-d9732dae9c19-1790953844561112000/target \
  --env HOMEBOY_PRODUCT_GIT_COMMIT=73d50cb21df95861133c1f236e31d10c203fce28 \
  --env HOMEBOY_PRODUCT_GIT_DIRTY=true \
  --artifact full-transport-evidence homeboy-lab -- timeout 2400 bash -c \
  'cargo build --quiet -p homeboy && python3 scripts/verify-lab-gate-transport.py "$CARGO_TARGET_DIR/debug/homeboy" full-transport-evidence /home/chubes/.config/homeboy/extensions/rust'
```

The product-identity environment is the existing snapshot-build provenance
contract in `homeboy-product-identity/build.rs`. The source head is the editing
checkout's actual `git rev-parse HEAD`; `dirty=true` truthfully identifies the
uncommitted source. Native admission still verifies actual image/lease identity.
No fake Git commits or source identity were manufactured.

- Complete native integration test: **1 passed, 0 failed, 0 ignored**, 2217
  filtered; 314.56 seconds of execution. The explicitly selected native test
  uses `--ignored --exact`; it is not skipped by this command.
- Actual installed Rust extension: **2 passed, 0 failed**, valid runtime
  inventory evidence. Lint/fmt runs normally. The fixture tracks its generated
  Cargo.lock so Git-backed materialization retains the inventory's lock input.
- Remote-only declared executable readiness was actually probed.
- Native package readiness succeeded with the declared digest; altered digest
  and missing executable were rejected as unavailable readiness.
- Probe receipts are **Skipped/readiness-only**, not passing executed gates.
- Private command and environment data were absent from public native job
  projections. Request/resources use existing owner-only hash-verified `@files`,
  not inline JSON argv. The direct daemon driver now shares the reverse worker's
  private input verifier and retains verified snapshots through child reaping.
- Native candidate materialization uses existing **SnapshotGit**, preserving the
  canonical HEAD/base/tree instead of an archive-only non-Git directory.
- Baseline replay succeeded from normalized **original controller sources**.
  Actual runner package paths remain in separate materialization evidence.
- Controller candidate mutation and changed declared closure were rejected.
- Cancellation reached the native job owner; the actual spawned sleep child
  disappeared from `/proc` before the assertion passed.
- A genuinely stale loaded fixture image and an absent runner were refused
  without gate execution. The fixture replaces only its own loaded image path;
  selecting a different compatible binary is correctly not treated as staleness.
- Executed red and deferred outcomes remained non-passing.
- A 100,000-byte output exercise preserved bounded capture/truncation evidence.
  Full terminal reports are private, digest-verified artifacts; native stdout
  carries only a compact receipt location/digest/status. The native terminal
  observer delivered **9 bounded gate heartbeat callbacks** through the existing
  gate-supervision callback boundary (the production Cook callback owns its
  durable projection).
- Authoritative terminal job state/exit code must agree with passing receipts;
  admitted job/controller-run identities are checked. Existing native terminal
  projection/artifact owners retain completed and cancelled results.
- In-memory plan steps are reconstructed from verified terminal reports rather
  than inheriting their serde-skipped default.

Retained final artifact: `629c8c06-ec2c-4ad3-92b8-5ce3596795a0`, kind
`full-transport-evidence`, directory SHA-256
`3269584c9fcbc05520b3b7523e72863e8516935fccdb62665a842e679205e0dd`.
It contains `verification-summary.json`, individual
case reports, real public job projections, receiver identity/SSH logs and copied
fixture-owned custodied receipt/diagnostic artifacts. All earlier failed native
attempts remain recorded; none is described as passing proof.

```sh
homeboy runs evidence fix-15359-full-transport-15
homeboy runs artifacts fix-15359-full-transport-15
homeboy runs artifact get fix-15359-full-transport-15 full-transport-evidence
```

### Final native owner checks

Run `fix-15359-owner-verification-final`, job
`f5cf99bd-3fdb-444b-ae7e-74639c94e72c`, artifact
`dde15105-5289-466c-854c-94e32676f63b` (`owner-verification-evidence`) used the same
snapshot/target/build-identity arguments above, with:

```sh
timeout 2400 python3 scripts/verify-gate-owner-tests.py owner-verification-evidence
```

Exact expanded commands and exit codes are retained in
`commands-and-outcomes.json`:

| Native selector (`cargo test --quiet -p PACKAGE --lib SELECTOR -- --test-threads=1`) | Counts |
| --- | --- |
| `homeboy-agents`, `agent_task_gate::placement::tests::` | 2 passed, 0 failed |
| `homeboy-cli`, `admitted_placement_tests` | 1 passed, 0 failed |
| `homeboy-lab-runner`, `private_at_file` | 7 passed, 0 failed |
| `homeboy-lab-runner`, `remote_daemon_status_` | 2 passed, 0 failed |
| `homeboy-agents`, `agent_task_gate::tests::` | **78 passed, 1 failed** |

The last command remains nonzero because
`rust_gate_cache_hydrates_once_coordinates_waiters_and_separates_identities`
rejects unsafe cache-root ownership/permissions. This was present in the initial
baseline and is not waived. The wrapper itself returns nonzero when any check
fails. Broad CONFIG_ROOT isolation #15328 and high-volume event projection
#15362 remain independent, unresolved work; this fixture's passing bounded
projection does not claim either broad issue is fixed.

### Owning API repairs discovered by real execution

1. Cold compact daemon status always emits a `daemon` object even when its
   address/lease/PID are null. The native status adapter treated that desired-build
   summary as a live daemon, refusing pristine startup as unreachable. The
   existing `connection/remote_daemon.rs` adapter now distinguishes absence from
   reachable/running ambiguous ownership and continues to fail closed for the
   latter. Actual producer evidence is retained in run
   `fix-15359-full-transport-7` as `bootstrap-before-status.stdout.json`:
   `running=false`, `reachable=false`, null address/lease/PID, `lease_missing`.
   Native cold startup succeeds in the final fixture without a forged session.
2. Installed shared-asset metadata used ambient config roots for special assets.
   The shared source owner now resolves against the **declared installation**;
   gate copying retains the original installed-path anchor before canonicalizing
   file contents. This permits a private controller to select an installed Rust
   closure without copying its operator registry. This is a narrow source-owner
   correction, not a waiver/fix of broad #15328.

The transport-15 delta changed/added **23 Rust source paths** (including fixture/wiring
updates and the native verification module), three native verifier scripts and
this evidence note. Central production scope is gate contract/placement,
promotion/pre-provider readiness, native private-file transport/custody, source
closure normalization and the two owning adapter corrections above. No new
lifecycle, queue or orchestration service was added.

## Historical entry-point-only evidence

AI assistance: OpenAI GPT-6.1 Sol via OpenCode. No delegation, commit, push, PR,
release, upgrade, or runner runtime-pin changes were performed.

## Cause and owner reuse

The existing gate executor selected isolated HOME/XDG before executing a nested
Homeboy command. A gate declaring Lab placement consequently attempted admission
through its private, deliberately unconfigured controller registry. Startup
diagnostics and truthful terminal-wait behavior do not supply runner authority.

This source candidate moves gate placement ahead of environment selection:

- The existing CLI parser and `gate_contract` simple-invocation recognizer
  resolve explicit Homeboy Lab gates. CLI resolver hooks retain parser ownership.
  `--gate-runner` also persists explicit placement in the existing gate contract.
- `runner_admission_snapshot` checks accepting/fresh admission before staging.
- `RunnerExecRequest`, `exec_request`, and native workspace snapshot
  materialization retain their existing job/lifecycle ownership.
- `observe_daemon_job_until_terminal` and `runner_job_cancel` implement terminal
  observation and cancellation; no replacement queue or lifecycle was added.
- `RunnerJobExecutionContext::from_direct_daemon_child_environment` authenticates
  the running daemon reservation before the runner gate selects private HOME.
  The existing Lab subprocess marker is then projected from that verified
  context so nested Homeboy commands execute on their resident runner.
- Existing gate process containment, environment selection, Rust result/count
  parsing, and declared extension/shared-asset copy/identity checks execute gates.
- Existing `candidate_fingerprint` and immutable promotion checkout proof bind
  HEAD/base/tree/destination. Lab promotion gates use the actual destination;
  terminal Lab tree evidence must match the canonical promotion checkout.
- The existing observation artifact owner retains controller-consumed terminal
  receipts. Receipts carry the admitted job context, invocation digest and
  candidate proof; promotion gate reports retain compact Lab receipt evidence.

Local gates remain the default. Private gate HOME receives declared extension
copies and shared closure, not an operator registry or credentials. Lab gates
use a replace-mode environment containing declared variables/preserve mappings
and the verified runtime's executable search path. A protocol test sets an
undeclared operator credential and verifies that the executed gate cannot see it.

## Verified identities

At initial readiness inspection, controller, configured Lab binary and admission
daemon reported:

`homeboy 0.399.13+c066ece3da11a476310d8792e8c84ec7d0210e2c`

Lab `homeboy-lab` was connected/fresh/accepting with 0/32 active jobs. Its admission
lease was `683afa46-7582-4778-9262-a05b788089bf`. Native checks built the candidate
under an owned Lab snapshot and invoked that build directly; they did not select
it as the published/runtime-pinned executable.

## Initial entry-point native command (historical)

```sh
homeboy runner exec \
  --run-id fix-15359-placement-native-7 \
  --sync-workspace /Users/chubes/Developer/homeboy@fix-15359-admitted-gate-placement \
  --env CARGO_TARGET_DIR=/home/chubes/Developer/_lab_workspaces/homeboy-fix-15359-admitted-gate-placement-a9af4c4cf18e-a84ecbd0-0e2b-4537-950e-d9732dae9c19-1790953844561112000/target \
  --artifact native-gate-evidence homeboy-lab -- timeout 2400 bash -c \
  'cargo test --quiet -p homeboy-agents --lib agent_task_gate::placement::tests:: -- --test-threads=1 && cargo test --quiet -p homeboy-cli --lib admitted_placement_tests -- --test-threads=1 && cargo build --quiet -p homeboy && python3 scripts/verify-admitted-lab-gate.py "$CARGO_TARGET_DIR/debug/homeboy" homeboy-lab native-gate-evidence'
```

- Run: `fix-15359-placement-native-7`.
- Native daemon job: `afa72a94-af54-43f4-8345-06f918922ccd`.
- Protocol tests: **2 passed, 0 failed, 0 ignored**, 2654 filtered.
- CLI placement-parser test: **1 passed, 0 failed, 0 ignored**, 3141 filtered.
- Candidate binary compilation: succeeded on Lab.
- Live admitted Rust gate: **2 passed, 0 failed**, selected/total **2**.
- Deliberately red Rust gate: **1 passed, 1 failed**, nonzero terminal receipt.
- Altered candidate rejected before execution.
- Declared shared-closure drift rejected before execution.
- Execution without a live runner reservation rejected before execution.
- Private HOME differs from operator HOME; tests read the declared private
  extension and shared-resource copies.
- Retained artifact: `b0256fee-5312-4a9b-932d-99de41bd1e09`.

Evidence readers:

```sh
homeboy runs evidence fix-15359-placement-native-7
homeboy runs artifacts fix-15359-placement-native-7
homeboy runs artifact get fix-15359-placement-native-7 native-gate-evidence
homeboy runner job logs homeboy-lab afa72a94-af54-43f4-8345-06f918922ccd
```

The directory contains passing, deliberately failing, candidate-tampering,
closure-drift and unadmitted-execution output. These are actual executed receipts,
not synthetic green command-result shapes.

## Broader failure retained, not waived

Before changes, run `fix-15359-gate-owner-baseline`, job
`7d7557be-f10c-4300-80e7-d03d3388dd23`, ran:

```sh
timeout 2400 cargo test -p homeboy-agents --lib agent_task_gate::tests:: -- --test-threads=1
```

Result: **78 passed, 1 failed, 0 ignored**, 2575 filtered.

The suite was repeated on the native-5 candidate snapshot with:

```sh
homeboy runner exec --run-id fix-15359-final-gate-suite \
  --cwd /home/chubes/Developer/_lab_workspaces/homeboy-fix-15359-admitted-gate-placement-9e3b5851f75c-48fd02f4-a70b-4ad2-bb7f-7dcb809eefce-1790957032072917000 \
  --env CARGO_TARGET_DIR=/home/chubes/Developer/_lab_workspaces/homeboy-fix-15359-admitted-gate-placement-a9af4c4cf18e-a84ecbd0-0e2b-4537-950e-d9732dae9c19-1790953844561112000/target \
  homeboy-lab -- timeout 2400 cargo test --quiet -p homeboy-agents --lib agent_task_gate::tests:: -- --test-threads=1
```

Run `fix-15359-final-gate-suite`, job
`279d60fe-a452-4232-8f97-be5b0bda3a61`: **78 passed, 1 failed, 0 ignored**,
2577 filtered. Both runs failed
`rust_gate_cache_hydrates_once_coordinates_waiters_and_separates_identities`:
`Rust gate cache root has unsafe ownership or permissions`. The broader
CONFIG_ROOT leak tracked in #15328 remains unresolved; no cache permissions or
operator state were changed to make this test green.

Earlier compile/parser iterations are retained under
`fix-15359-placement-compile-{1,2}` and `fix-15359-placement-native-{1,3,4}`.
They exposed source visibility/missing-default issues and an invalid test
declaration combining mutually exclusive `--runner`/`--placement`; those were
corrected. When execution failed before creating declared evidence,
`runner exec` additionally returned an artifact-path-not-found error. The native
job logs retain the underlying failures; these attempts are not passing proof.
No oversized event-append failure occurred in the successful final native run.

## Current release/acceptance boundary

The six former editing-session transport gaps above are superseded by the full
native proof and owner checks in the continuation section. Supervisor owns
source acceptance, release, post-release untouched published-runtime proof and
all finalization. The selected operator runtime remains unchanged. The broader
cache/config-root failure and #15362 are still recorded independent blockers,
not waived as part of this gate repair.

## Source overlap and scope

The initial candidate changed fourteen Rust source files; the continuation's
final scope is recorded above. The central
production changes are `agent_task_gate.rs`, its new `placement.rs`, promotion's
`promote.rs`, the native runner's new `gate_transport.rs`, and CLI parser/startup
wiring. No source edits were made to native terminal-wait/daemon observation
owners or the #15358 startup diagnostic implementation.

Shared paths for the supervisor to compare against #15356 before merging include
`crates/homeboy-cli/src/cli_runtime.rs`,
`crates/homeboy-cli/src/commands/agent_task.rs`,
`crates/homeboy-cli/src/commands/agent_task/gate_contract.rs`,
`crates/homeboy-agents/src/agent_task_gate.rs`, and
`crates/homeboy-lab-runner/src/lib.rs`,
`crates/homeboy-lab-runner/src/connection/remote_daemon.rs`,
`crates/homeboy-lab-runner/src/daemon_exec_driver.rs`, and
`crates/homeboy-lab-runner/src/worker/run.rs`. The editing session did not inspect or
merge the separate uncommitted #15356 checkout.
