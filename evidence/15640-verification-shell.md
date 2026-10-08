# #15640 — environment-preserving verification shell

Owning issue: <https://github.com/Extra-Chill/homeboy/issues/15640>.
Read with `gh issue view 15640 --json title,body,url` from the owned worktree.

## Source and consolidation

- Branch: `fix/15640-verification-shell`.
- Immutable production base: `11dfdd86cd643e143eb768a295ef559d99e94957`.
- Candidate: **uncommitted source changes**, not a newly minted candidate commit.
- Final reviewed source-diff SHA-256, including the follow-up Action engineering
  test: `60a41176dfa9b050342367ec714849d944d76779e1a39c97fa7fac05f34d684e`.
  Computed with `git diff --binary -- crates/homeboy-agents crates/homeboy-cli | shasum -a 256`.
  This excludes this evidence document and generated verification directories.
- Named owner: `agent_task_gate::legacy_gate_argv`, exported through
  `agent_tasks::gate::legacy_gate_argv`. A fresh implicit legacy program is
  exactly `["sh", "-c", source]`: noninteractive, non-login POSIX sh.
- Candidate supervision, bounded baseline execution, promotion skipped and
  unavailable reports, controller command gates, and manual verification
  execution/reporting consume that primitive. Report inference is consolidated
  in `recorded_legacy_gate_invocation`.
- The existing verification kernel still selects the environment through
  `selected_gate_environment`, probes declared tools directly, and supervises
  real children through its existing process-tree/capture implementation.
  There is no project-specific PATH repair.

The original handoff's source-diff SHA-256 was
`b231fbd7970b917b4b0648ce10185ce2fd6f68aa380aba7da2e3eb1c2a422be6`.
The follow-up adds only an opt-in test to that source diff; production shell,
environment, identity, and reporting behavior is unchanged.

The original replacement-test job records the actual uncommitted input:
`snapshot:93d14de8a8055193`, content hash
`sha256:24d1cb1614adb4676d0fa87360d8f165862ae1ea0fd0979d9114ddfe63552155`,
branch `fix/15640-verification-shell`, base SHA above, `dirty: true`.
That workspace snapshot also contained the owned auxiliary immutable-base
checkout used for engineering verification; that local checkout was removed
after verification. Runner-retained snapshots and historical evidence were
not rewritten. The source-diff digest above identifies the handed-off changes
independently of those auxiliary bytes.

## Invocation identity and compatibility

`LegacyGateShell` records the shell contract inside a legacy invocation:

- Missing serialized `shell` means `HistoricalLogin`. Its digest remains the
  historical NUL-separated digest of `sh`, `-lc`, and the original source.
  Serializing that default still omits the field.
- Fresh reports explicitly record `shell: "non_interactive"` and their concrete
  command is `sh -c`. Their digest covers `sh`, `-c`, and the source, so fresh
  execution cannot masquerade as historical login-shell proof.
- `DeclaredTest` plans retain their argv/deadline digest and direct execution.
  Explicit argv entering the shared executor is never rewritten: the subprocess
  test runs both `sh -lc` and `bash -lc`, checks their exact recorded argv, and
  round-trips the reports. This does not expand the declared review-test plan's
  accepted grammar.
- Reports predating the invocation field deserialize with their recorded
  `sh -lc` command intact. Their inferred digest is historical, not upgraded.
- Immutable-baseline comparison checks invocation identity before comparing
  failures. A historical-login candidate replayed with today's fresh shell is
  **inconclusive**, `matches_candidate_failure: false`, and requires candidate
  recapture. Identical stderr is insufficient to accept that changed contract.

The original `legacy_shell_invocation_keeps_the_historical_gate_command_digest`
assertion is retained and strengthened with missing-field deserialization and
an explicit assertion that fresh and historical digests differ. Source and
candidate checkout binding continue to be exercised by the promotion tests.

## Real before/after proof

`legacy_gate_preserves_declared_path_at_readiness_candidate_and_baseline`
creates an executable named `bash` in its private fixture bin directory. Its
declared PATH puts that directory first. The executable checks isolated HOME
and XDG initialization and records the path observed by readiness. Candidate
and bounded-baseline execution each assert that direct lookup, direct execution,
child-shell lookup, and child-shell execution return the same readiness path.
Both gate reports must be genuinely succeeded/completed, with matching
invocation digests. No inherited-failure acceptance is used for this proof.

The production baseline was a detached checkout of the immutable base, with
**only this regression test added** using `apply_patch`.

| Platform / production | Actual result |
| --- | --- |
| macOS, immutable base, final shadowed-tool fixture | **0 passed, 1 failed**, Cargo exit 101. Readiness observed `<fixture>/bin/bash`; candidate and child-shell lookup returned `/bin/bash`, so the four-line output assertion failed semantically. |
| macOS, candidate, ordinary PATH fixture | **1 passed, 0 failed**; direct and child execution agree with readiness for both gate owners. |
| macOS, candidate, real initialized Bash proof | **1 passed, 0 failed**; both candidate and bounded baseline, including child shells, ran Bash `5.3.9(1)-release` and successfully checked `BASHPID`. |
| Linux Lab, immutable base, final shadowed-tool fixture | **1 passed, 0 failed**. This host's login shell did not reorder the declared PATH; this is not claimed as a Linux reproduction of the macOS defect. |
| Linux Lab, candidate, ordinary PATH fixture | Passed within the **105/105** owning gate-kernel run. |

The real Bash proof is an explicit opt-in macOS engineering test because a
default macOS installation provides Bash3 rather than the initialized Bash4+
toolchain needed for this particular proof. It was actually run with `--ignored`
and passed; the ordinary PATH regression has no Bash4+ prerequisite.

A separate native subprocess comparison also confirmed the exact original
toolchain disparity. With HOME and XDG_CONFIG_HOME rooted under the owned
worktree's `target/15640-shell-proof`, and identical declared
`PATH=/opt/homebrew/bin:/usr/bin:/bin`, execute this same program with either
`sh -lc` or `sh -c`:

```sh
command -v bash; bash --version; bash -c 'test -n "$BASHPID"'
```

Python `subprocess.run(..., capture_output=True, text=True, timeout=10)` recorded:

```text
-lc exit 1
/bin/bash
GNU bash, version 3.2.57(1)-release (arm64-apple-darwin26)

-c exit 0
/opt/homebrew/bin/bash
GNU bash, version 5.3.9(1)-release (aarch64-apple-darwin25.1.0)
```

No login profiles were edited. The first exploratory fixture used a unique
executable basename and passed on macOS too: PATH was reordered, not completely
erased. Shadowing a system basename made the regression detect the actual
selection bug. The first Linux test build also found a test-field typo
(`isolated` instead of the existing `sanitized` evidence field); it was corrected
before claiming any semantic result.

## Actual verification commands and receipts

`OWNED_WORKTREE` below denotes the operator-supplied isolated worktree, and
`BASE_WORKTREE` denoted its temporary `.tmp-test/immutable-base` detached checkout.
All source operations stayed inside the permitted outer workspace.

Each Linux row used this actual command structure, sequentially against the
exclusive target (substitute the row's workspace, run ID, and Cargo suffix):

```sh
homeboy runner exec --sync-workspace "$WORKSPACE" \
  --workspace-sync-timeout 900s --run-id "$RUN_ID" homeboy-lab -- \
  env RUST_MIN_STACK=16777216 CARGO_BUILD_JOBS=2 \
  CARGO_TARGET_DIR=/home/chubes/.local/share/homeboy/cargo-targets/fix-15640-verification-shell \
  cargo test --locked -p "$CRATE" --lib "$FILTER" -- $HARNESS_ARGS
```

| Run ID | Daemon job ID | Workspace; crate; filter; harness arguments | Result |
| --- | --- | --- | --- |
| `fix-15640-base-path-20261008-01` | `c6b3ad16-d79c-4ba0-b1fc-31a101b15d43` | Base; `homeboy-agents`; `legacy_gate_preserves_declared_path_at_readiness_candidate_and_baseline`; `--nocapture` | Build-only failure, exit 101, test-field typo; **not** semantic baseline evidence. |
| `fix-15640-base-path-20261008-02` | `d5124c5d-b4bb-49b7-a8e3-629932c0d279` | Base; same suffix | Preliminary unique-basename fixture: 1/1 passed, exit 0. |
| `fix-15640-base-path-20261008-03` | `dfeef9a6-6559-4ff2-8eb0-efa0809dc0e0` | Base; same suffix | Final shadowed-tool fixture: 1/1 passed, exit 0. |
| `fix-15640-candidate-gate-20261008-01` | `6825ab6d-e500-42ab-988e-687677c01b42` | Candidate; `homeboy-agents`; `agent_task_gate`; `--test-threads=1` | 105 passed, 0 failed, exit 0. |
| `fix-15640-candidate-promotion-20261008-01` | `cbcaa505-c0a8-4d9f-ab72-b992dada3de3` | Candidate; `homeboy-agents`; `agent_task_promotion`; `--test-threads=1` | 112 passed, 0 failed, exit 0. |
| `fix-15640-candidate-baseline-20261008-01` | `8f33ffaf-8e23-452f-83e0-bcf17e8fd88c` | Candidate; `homeboy-agents`; `cook_baseline`; `--test-threads=1` | 7 passed, 0 failed, exit 0. |
| `fix-15640-candidate-manual-20261008-01` | `b36a8f1c-5560-41c7-92f8-0c64e8c9092c` | Candidate; `homeboy-cli`; `commands::agent_task::review::tests`; `--test-threads=1` | 58 passed, 0 failed, exit 0. |
| `fix-15640-candidate-controller-20261008-01` | `93d1634a-dab0-47db-9408-e0ec39aef4b6` | Candidate; `homeboy-agents`; `run_gates_tests`; `--test-threads=1` | 6 passed, 0 failed, exit 0. |
| `fix-15640-candidate-replacement-20261008-01` | `5a67b95e-df9d-4992-a38f-2909d291ef2d` | Candidate; `homeboy-agents`; `verify_replacement_gates_recovers_pending_verification_and_replays_completed_proof`; `--nocapture --test-threads=1` | 1 passed, 0 failed, exit 0. |

Authoritative evidence is the daemon job result/log, accessible with:

```sh
homeboy runner job logs homeboy-lab "$JOB_ID" --compact --tail 8
```

The first long runner command exceeded the controller tool's 120-second wait;
its durable job was located and inspected, rather than duplicating a running
build. Later commands used a 900-second controller wait. No baseline/candidate
builds contended for the Linux Cargo target. Native builds used a separate
owned local target, also sequential for baseline and candidate.

Native Cargo commands, executed from the respective owned checkout:

```sh
# Base: final fixture failed semantically, 0 passed / 1 failed.
env RUST_MIN_STACK=16777216 CARGO_BUILD_JOBS=2 \
  CARGO_TARGET_DIR="$OWNED_WORKTREE/target/15640-native" \
  cargo test --locked -p homeboy-agents --lib \
  legacy_gate_preserves_declared_path_at_readiness_candidate_and_baseline -- --nocapture

# Candidate: PATH and real Bash proof both passed (2/2 before opt-in annotation).
env RUST_MIN_STACK=16777216 CARGO_BUILD_JOBS=2 \
  CARGO_TARGET_DIR="$OWNED_WORKTREE/target/15640-native" \
  cargo test --locked -p homeboy-agents --lib legacy_gate_preserves_ -- --nocapture

# Final candidate: explicit initialized-Bash engineering proof passed 1/1.
env RUST_MIN_STACK=16777216 CARGO_BUILD_JOBS=2 \
  CARGO_TARGET_DIR="$OWNED_WORKTREE/target/15640-native" \
  cargo test --locked -p homeboy-agents --lib \
  legacy_gate_preserves_initialized_bash_toolchain_and_bashpid -- --ignored --nocapture
```

The first four candidate Linux receipts precede only a documentation-comment
update and the macOS proof's opt-in annotation. Controller/replacement receipts
compile the original handed-off source diff; no production behavior was changed
after the gate-kernel proof. The follow-up Linux receipt below compiles the
source with the new external Action test. `cargo fmt --all --check` and
`git diff --check` also passed.

## Self-review, coverage, and limitations

- Owning gate tests exercise real exit/failure classification, bounded capture
  metadata, private redaction and live-output privacy, deadlines, cancellation,
  no-progress detection, spawn-registration failure, and descendant reaping
  including controller death. Typed candidate/baseline argv/deadline outcomes
  remain tested. Promotion tests exercise immutable candidate binding, ordered
  skips, unavailable gates, and destination execution.
- Baseline comparison's historical-contract case proves that identical failure
  text cannot excuse a shell-identity change. Existing explicitly authorized
  inherited-failure tests retain their original policy assertions; none substitute
  for the successful PATH/BASHPID subprocess proofs.
- Fresh test-provider/report expectations were updated; persisted historical
  fixture commands and the historical digest expectation were retained.
- Remaining login-shell sites were classified: provider-ref **setup** commands,
  runner upgrade/refresh/bootstrap/diagnostic commands, application launchers,
  explicitly supplied argv, and historical report fixtures are separate from
  framework-created verification programs.
- No Rust-extension parity override was needed. None of these focused runs
  encountered the known inherited SQLite/free-disk fixture failures. The native
  build emitted an existing `homeboy-core` unused-function warning.
- This slice proves the verification-shell boundary and real Bash5/BASHPID
  behavior. The follow-up below additionally runs the unchanged actual Action
  release-wrapper suite. It does not claim a new released Homeboy binary, the
  entire aggregate Action shell/Python test inventory, or Action527's mandatory
  live release proof. Live release verification remains separate.
- No commits, pushes, PRs, GitHub comments, or merges were performed.

## Follow-up — actual Homeboy Action release-wrapper suite

The operator requested the actual Action wrapper test run before publication.
The permitted reference was prepared with:

```sh
git clone https://github.com/Extra-Chill/homeboy-action.git \
  "$OUTER_WORKSPACE/.tmp-test/homeboy-orchestration/action-15640-reference"
```

Only that owned reference checkout was read. It was not patched. Source identity:

- Action commit: `e598b0f6c53407b2680517ff36802daabdc30947`.
- Action `VERSION`: `2.20.17`.
- Homeboy base: `11dfdd86cd643e143eb768a295ef559d99e94957`, with the uncommitted
  implementation and engineering test identified by the final source-diff hash.
- `git status --porcelain=v1 --untracked-files=all` was empty before and after
  execution; `git diff --exit-code` passed. The reference is retained at the
  permitted path and has been made filesystem read-only (`chmod -R a-w`).

### Discovery and exact test command

The Action has no package test command. Its actual aggregate runner is
`bash scripts/run-tests.sh`, as declared in `.github/workflows/self-test.yml`.
That runner discovers `scripts/*/test-*.sh` and `scripts/*/test-*.py`, and includes
the specific existing release-wrapper suite:

```sh
bash scripts/release/test-run-release-wrapper.sh
```

The suite directly exercises the unchanged `scripts/release/run-release.sh` and
`scripts/release/run-release-with-liveness.sh`. Its source lines 605–678 include
the planning-liveness case implicated by #15640, release execution, expected
timeout classification/descendant cleanup, and failure-summary checks. The
suite's own existing fixture programs mock `git`, `homeboy`, and `gh`; its
"Released"/"Verified GitHub Release" notices describe those local fixtures,
not a real release. No release, publication, deployment, or GitHub write occurred.
The existing Action fixture policy was not changed, including its advisory
liveness selection. Homeboy's enclosing real process supervisor and isolated
gate environment remained active.

### Execution through both corrected gate owners

Added opt-in test:
`agent_task_gate::tests::legacy_gate_runs_unchanged_action_release_wrapper_suite`.
It reads the reference source, checks checkout cleanliness, probes declared
Bash4+/jq/Python readiness, and runs the **same unchanged suite** twice:

1. `run_gate_command_with_supervision`, with a 120-second wall-clock and
   no-progress bound.
2. `run_gate_command_with_timeout`, with a 120-second baseline wall-clock bound.

The two runs receive separate owned runtime directories. Both use
`AgentTaskGateEnvironmentMode::Replace`, isolated HOME and all XDG directories,
and exactly the caller-declared `PATH=/opt/homebrew/bin:/usr/bin:/bin`. No ambient
credentials are needed. Their concrete recorded argv is identical:

```json
["sh", "-c", "bash scripts/release/test-run-release-wrapper.sh"]
```

The readiness probes ran in each owner's selected environment. The captured
toolchain gate additionally observed direct and child-shell selection of
`/opt/homebrew/bin/bash`, GNU Bash `5.3.9(1)-release`; jq `1.8.1`; Python `3.9.6`.
Bash readiness explicitly checked `BASHPID` before each wrapper run.

Actual native command, from the owned Homeboy worktree:

```sh
env RUST_MIN_STACK=16777216 CARGO_BUILD_JOBS=2 \
  CARGO_TARGET_DIR="$OWNED_WORKTREE/target/15640-native" \
  HOMEBOY_TEST_ACTION_REFERENCE="$OUTER_WORKSPACE/.tmp-test/homeboy-orchestration/action-15640-reference" \
  HOMEBOY_TEST_ACTION_TOOLCHAIN_PATH=/opt/homebrew/bin:/usr/bin:/bin \
  HOMEBOY_TEST_ACTION_EVIDENCE_DIR="$OWNED_WORKTREE/target/15640-action-proof" \
  cargo test --locked -p homeboy-agents --lib \
  legacy_gate_runs_unchanged_action_release_wrapper_suite \
  -- --ignored --nocapture --test-threads=1
```

Observed Cargo result: **1 passed, 0 failed, exit 0**, 34.00 seconds test time.
The one Rust engineering test encompasses both real Action-suite executions:

| Gate owner | Observed Action checks | Gate result | Capture |
| --- | --- | --- | --- |
| Candidate supervised executor | 92 `PASS:` lines, 0 `FAIL:` lines, final `All run-release wrapper checks passed.` | `succeeded`, exit 0, `completed` | 8,754 stdout bytes retained; 0 stderr bytes; no truncation |
| Bounded baseline executor | 92 `PASS:` lines, 0 `FAIL:` lines, same final sentinel | `succeeded`, exit 0, `completed` | 8,754 stdout bytes retained; 0 stderr bytes; no truncation |

These are **92 observed assertion/check lines per suite execution**, not an
invented count of independent test files. Both streams have SHA-256
`f201543390b97519a38bc5f045f4b7f65df7c71d2d88bdb356055403c3e66a8c`.
The engineering test also verifies identical concrete commands and invocation
digests. No inherited-failure acceptance participates in the result.

Failure and timeout behavior remained exercised by the unchanged suite:

- Planning emitted the expected heartbeat and owning 1-second budget warning.
- The mock 3-second release timeout was required to return 124, and its
  descendant was required to be gone. The suite emitted
  `PASS: release liveness timeout cleans descendants without mutation`.
- The expected classified release-failure scenario remained nonzero internally
  and produced a summary with exit code 1 and the `gh-upload-failed` diagnostic.
- Neither enclosing 120-second Homeboy gate timed out or failed. There were no
  independent Action-suite failures requiring direct/baseline diagnosis.

Machine-readable native receipts are retained under the owned worktree:

```text
target/15640-action-proof/source.json
target/15640-action-proof/toolchain-report.json
target/15640-action-proof/candidate-report.json
target/15640-action-proof/baseline-report.json
```

The captured reports include exact source/command facts, selected environment,
exit/termination, bounded stream metadata, and the full wrapper-check output.
Temporary runtime directories were cleaned when the engineering test finished.

### Final Linux compilation and kernel regression check

After the native Action proof, the final source was verified again on Linux
using the required exclusive target, sequentially after all native Cargo work:

```sh
homeboy runner exec --sync-workspace "$OWNED_WORKTREE" \
  --workspace-sync-timeout 900s --run-id fix-15640-followup-gate-20261008-01 \
  homeboy-lab -- env RUST_MIN_STACK=16777216 CARGO_BUILD_JOBS=2 \
  CARGO_TARGET_DIR=/home/chubes/.local/share/homeboy/cargo-targets/fix-15640-verification-shell \
  cargo test --locked -p homeboy-agents --lib agent_task_gate -- --test-threads=1
```

Authoritative daemon job: `99e71b7c-32e2-4b6f-99bb-db35037e6f85`.
Result: **105 passed, 0 failed, 1 ignored, exit 0**, 19.20 seconds test time.
The ignored test is the newly added external-checkout engineering proof, which
was explicitly executed and passed natively above. All ordinary gate tests,
including typed/historical compatibility, privacy, capture, and bounded real
subprocess/descendant cleanup, pass with the final source.

Self-review found no additional production changes in this follow-up. The
unchanged actual wrapper-suite requirement is now verified. **Action527's
mandatory live release proof remains separate and is not satisfied or waived
by this mocked wrapper-suite run.**
