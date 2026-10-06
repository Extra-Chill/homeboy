# Cause-only full-evidence retention — final report

## Scope and commits

- Branch: `fix/15156-transport-cause-publish`; PR: [#15548](https://github.com/Extra-Chill/homeboy/pull/15548).
- Scoped starting merge: `c6e8e7d0bbf43a53af983f2047d0eb452edd3795` (parents `ab05a950e85bc7dc1af0771a661e5dbbb6c8f74f` and `df82bb7a854aaa2845ca9411f6df5020dfc72bd3`).
- Transplanted only the preserved cause-budget commit `5757fc43e6fa5ae0221525d54d078899594188b5`, as `5ebce6e89` (`fix(lab): separate cause from transport context`). The original diagnostics checkout was left untouched. No compact-presentation commit was included.
- Final code HEAD before this report: `358d27fc38aa750d80f3469b1f825278c417bac5`.
- Scoped commits after the starting merge, oldest to newest: `5ebce6e89574b1df3be6939dda105d865fd50cc5`, `d1e0c4fbc36e362ca4837e348473ecff9ff50c8d`, `75bb1c394dda1303ec49be83a818531284a40c6c`, `c4a8359243f37e2595447639e6c71cfae45e4974`, `baa422d7007bd58c97b2cf0bc445f053ebfafe1b`, `5adc836118c66c0ac131e3337b2f5b8cd7e0f1f2`, `9cb70d9a4178e31b96776bbd143ceba42feefa91`, `d3c3501bd1a949b75c74e1ac840ca09627894edc`, `fdb707a26adb2d4436964386495c0ddf427f83a3`, `afcdcd22b236969623a38029d0c126e92471a70e`, `bfd8def71642420d940f44cb0a14c38eed0f0866`, `496fd30f7e631552708cf0b3828499ca069e9583`, `358d27fc38aa750d80f3469b1f825278c417bac5`.
- Final integrated publication HEAD is recorded in the completion addendum below.

## Implementation

Changed files:

- `crates/contracts/homeboy-lab-contract/src/lab/transport_failure.rs`
- `crates/homeboy-lab-runner/src/transport.rs`
- `crates/homeboy-cli/src/commands/agent_task/status.rs`
- `crates/homeboy-cli/src/commands/agent_task/tests/lifecycle.rs`

The canonical transport wrapper now stores a recursively redacted `source_error` alongside the bounded receipt. It retains the source code, message, structured details, hints, retryability, and typed causes (including raw OS error codes when available) for durable evidence. Provider filesystem errors keep their original error classification and complete redacted operation/cause in details; HTTP and direct-SSH transfer errors preserve full redacted source details and paths while bounding the operator summary. The receipt keeps a 512-character message, bounded cause list, and independent bounded context; its v2 context field remains optional when reading v1 receipts.

Explicit `diagnose --full` now includes those complete source facts from the durable failure record, and `evidence --full` retains them through the existing record projection. Compact/default summaries remain bounded. Regression coverage uses a real missing-file failure through the provider evidence error constructor, a loopback HTTP 403 response with >4 KiB structured details and a >512-character cause/path, and a direct-transfer source with long stderr/path. Fixtures contain credentials non-vacuously and assert they are absent from persisted and projected surfaces.

## Linux verification

- Linux source: `/home/chubes/Developer/homeboy-15156-cause-publish`
- Linux target: `/home/chubes/Developer/homeboy-15156-cause-publish/target`
- Raw proof logs: `/home/chubes/Developer/homeboy-15156-cause-publish-evidence/`

| Check | Result | Raw evidence |
|---|---:|---|
| `cargo fmt --all -- --check` | Pass | `final/fmt-final.log` |
| `git diff --check c6e8e7d0b..HEAD` | Pass | final local status/diff review |
| `cargo test -p homeboy-lab-contract` | 59 passed | `final/lab-contract-final.log` |
| `cargo test -p homeboy-lab-runner transport::tests -- --test-threads=1` | 14 passed; 2,228 filtered | `final/lab-transport-final.log` |
| Focused `homeboy-cli` lifecycle/store regression | 1 passed; 3,166 filtered | `final/cli-evidence-final.log` |
| `cargo build -p homeboy --bin homeboy` | Pass | `final/current-main-binary-build.log` |
| `cargo test -p homeboy --test cli_binary -- --test-threads=1` | 53 passed, 2 failed | `final/cli-binary-final.log` |

The two binary-suite failures are inherited: `cook_preview_lifecycle::removed_detach_after_handoff_flag_is_rejected_before_cook_admission` and `cook_preview_lifecycle::unresolved_backend_preview_binds_stable_replay_lifecycle_without_mutation` each observe one entry in the temporary home where the test expects zero. The exact scoped base `c6e8e7d0bbf43a53af983f2047d0eb452edd3795` reproduces the same two failures with the same 53/2 counts: `base/cli-binary-base.log`.

The full-evidence regression also has a recorded red run at pre-fix projection revision `baa422d7007bd58c97b2cf0bc445f053ebfafe1b`: the real OS failure was durably recorded, but full diagnosis returned `null` rather than the complete redacted source context. See `base/diagnosis-projection-red.log`. The final focused test passes at the final code HEAD.

## Coordination evidence and remaining boundary

The named retained worktree and its report were no longer present after the supervisor stopped the session; the requested integration event/report files were also absent from the named Linux reference worktrees. The cause-budget diff was therefore transplanted from its preserved commit `5757fc43e6fa5ae0221525d54d078899594188b5`, and the focused Linux tests above provide final verification.

Receipt projections intentionally remain bounded (512-character message, 4 KiB context, four causes of at most 256 characters each). Complete source facts are retained in the explicitly durable `source_error` evidence; summaries remain the bounded default. Raw in-process `Error::source` values are not serialized directly—the durable projection redacts each source fact.

## Integrated publication addendum — 2026-10-06

- Updated the branch with an ordinary merge of current `origin/main` at `787eac0e504cb4e5f6320c071e9ae48538811410`; merge commit `87a4a321ad4bfc0da002aa235d2d3d7bcb10546d`. The merge incorporated intervening upstream changes without conflicts.
- Before the merge, authoritative CI run `37394971745` exposed two candidate-only regressions on its then-current base: the CLI lifecycle assertion expected the old actionable command projection, and the HTTP fixture expected authentication redaction behavior changed by upstream. Both targeted regressions pass against the merged current-main source; no test was skipped or weakened.
- Current local checks after integration: `cargo test -p homeboy-lab-contract --lib` (59 passed), `cargo test -p homeboy-lab-runner --lib transport::tests -- --test-threads=1` (14 passed, including real localhost HTTP 403 cause/redaction and actual missing-file/disk-full coverage), durable CLI lifecycle regression (1 passed), `cargo fmt --all -- --check`, and `git diff --check` all pass.
- Earlier Linux logs under `/home/chubes/Developer/homeboy-15156-cause-publish-evidence/final/` apply to source `358d27fc38aa750d80f3469b1f825278c417bac5`, not the integrated merge. Integrated Linux verification on the exact publication SHA is required and will be recorded in the final report update.
- Runtime evidence for this direct OpenCode session: session `ses_ef173deffffeKa2yxCUozu1yXA`, model `openai/gpt-6-luna`, in `/var/folders/lr/c_cmmt7s0592m4njz99v5yb40000gn/T/opencode/homeboy-15156-cause-final-runtime.log`; the retained completion event stream is `/var/folders/lr/c_cmmt7s0592m4njz99v5yb40000gn/T/opencode/homeboy-15156-cause-completion-events.jsonl`. PR disclosure identifies gpt-6-luna implementation and gpt-6.1-sol supervision.
- This remains a draft for independent parent review and authoritative green CI; no merge or ready-for-review transition is requested.
