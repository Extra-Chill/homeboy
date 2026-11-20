# Cook CLI run module split

Behavior-neutral structural slice of [#18436](https://github.com/Extra-Chill/homeboy/issues/18436), implementing [#18590](https://github.com/Extra-Chill/homeboy/issues/18590).

All paths below are relative to `crates/homeboy-cli/src/commands/agent_task/`.
Original line ranges refer to `run.rs` before this split.

| Original region | New home | Seam |
| --- | --- | --- |
| 48–490: identity, placement, rotation, readiness and failure disclosure | `run_disclosure.rs` | Projections |
| 680–1797: preview, capacity, replay argv and placement policy | `run_preview.rs` | Read-only projections |
| 4525–5686: continuation requests, preflight, execution and status | `run_continuation.rs` | State reads and transitions |
| 5687–6842: source history, provisioning, base/issue/PR refs | `run_destination.rs` | Destination adapters |
| 6843–7785: repository identity, workspace selection and branch derivation | `run_repository.rs` | Repository adapters |
| 7786–9299: request preparation, admission, dispatch, prompt snapshots and plan compilation | `run_execution.rs` | Execution transitions |
| 9300–11504: evidence admission, immutable storage, copying and prompt rewriting | `run_provider_evidence.rs` | Evidence adapters |
| Remaining command entry points, component gates, workspace attestation and retry admission | `run.rs` | Facade and lifecycle handlers |

Every original inline test module now uses a `#[cfg(test)]`/`#[path]` declaration in `run.rs`, with its body in `run_<module-name>.rs`. The original `run::tests` module lives in `run_tests.rs`. Preview tests also include `run_preview_admission_tests.rs` within the same module, preserving fixture visibility and full test names without exceeding the size limit.

## Size accounting

Counts include blank lines and comments. “Non-test” excludes inline test-module blocks (including their `#[cfg(test)]` attributes) before the split, and extracted test files plus their three-line module declarations afterward. As before, standalone test-only helper functions are included in the non-test count.

| Scope | Total lines | Non-test lines |
| --- | ---: | ---: |
| Original `run.rs` | 13,217 | 7,419 |
| New `run.rs` facade | 838 | 817 |
| All split run files, including facade and tests | 13,394 | 7,630 |

The largest new file is `run_preview_admission_tests.rs` at 1,686 lines. The small aggregate increase comes from module headers, imports, compatibility re-exports, visibility annotations and formatting; no helpers or tests were deleted. Function bodies remain unchanged apart from formatting.

## Compatibility

Existing public and crate-visible `run` paths are preserved through re-exports. Internal visibility is widened only as needed across sibling modules. CLI behavior, JSON output and test names are unchanged. No changes were made to `cook.rs`, `lifecycle_ops.rs` or `cook_run_state.rs`.

## AI assistance

Implemented with OpenAI assistance via Pi, directed by Chris Huber. The assistant extracted modules and tests, preserved compatibility paths, documented the region map, and ran focused checks. The issue was drafted by Claude via Roadie/Pi.
