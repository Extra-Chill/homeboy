# Gate module layout

Behavior-neutral slice of [#18436](https://github.com/Extra-Chill/homeboy/issues/18436), implementing [#18591](https://github.com/Extra-Chill/homeboy/issues/18591).

The parent module retains the state/contracts, report transitions, serde defaults,
and compatibility re-exports. Existing public and crate-visible import paths,
JSON shapes, CLI behavior, and test names are unchanged. No helpers were deleted.

## Region map

Original `agent_task_gate.rs` line ranges (before this split):

| Old region | New sibling |
| --- | --- |
| 629–853: dependency hydration and git-ignored child discovery | `dependency_setup.rs` |
| 1538–1648: diagnostic sidecar ingestion/normalization | `diagnostics.rs` |
| 1937–2182: failure fingerprints and output normalization | `failure_records.rs` |
| 2580–3408: command/argv execution and supervision | `execution.rs` |
| 3409–3492: controller extension selection | `extension_sources.rs` |
| 3493–4018: selected environment adapter | `environment.rs` |
| 4019–4752: Rust cache hydration, safety and eviction | `rust_cache.rs` |
| 4753–5469: isolated environment and extension materialization | `environment_setup.rs` |
| 5470–5969: toolchain and package-artifact preflight | `preflight.rs` |
| 5978–6786: failure classification and Cargo evidence | `failure_evidence.rs` |
| 6787–6993: core gate result and feedback projections | `projections.rs` |
| Inline `baseline_tests`, `toolchain_requirement_serde_tests`, `rustup_location_tests` | Same-named sibling test files |
| Inline `tests` | `tests.rs`, including four bounded topic fragments under `tests/` |

The test fragments deliberately share the original `tests` module: shared helpers
and fully qualified test names remain stable. Two `include_str!` fixture paths
are adjusted only for their new directory depth. `gate_runtime` stays a sibling.

## Line counts

Counts cover only the original entry file and files extracted from it (not
pre-existing sibling modules). Blank lines and comments count.

| Scope | Before total | Before non-test | After total | After non-test |
| --- | ---: | ---: | ---: | ---: |
| Entry file | 13,766 | 6,542 | 1,726 | 1,662 |
| Entry plus extracted files | 13,766 | 6,542 | 14,188 | 6,954 |

Non-test counts exclude test-only modules, the test fixture helper, test-only
execution wrappers, and their cfg/re-export declarations. Module wiring,
imports and formatting account for the increase; no runtime logic was added.
The largest new Rust file is 1,886 lines; the entry file is below 2,000.

## AI assistance

AI moved existing regions and tests, adjusted module-boundary visibility and
fixture paths, and checked compilation, formatting and focused baseline tests.
