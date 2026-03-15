# Core runner + output parse substrate

This document defines the core primitives introduced for:

- #460 — extension runner helper contract
- #464 — generic output parsing primitive

## Runner contract (core)

`src/core/extension/runner_contract.rs`

- `RunnerStepFilter { step, skip }`
- `should_run(step_name)` for deterministic include/skip semantics
- `to_env_pairs()` maps to exec context vars:
  - `HOMEBOY_STEP`
  - `HOMEBOY_SKIP`

`execution.rs` now aliases legacy `ExtensionStepFilter` to `RunnerStepFilter` to keep command API
stable while moving behavior to a reusable core primitive.

## Output parse primitive (core)

`src/core/engine/output_parse.rs`

Generic parser with declarative rule spec:

- `ParseRule { pattern, field, group, aggregate }`
- `DeriveRule { field, expr }`
- `ParseSpec { rules, defaults, derive }`
- `parse_output(text, spec) -> HashMap<String, f64>`

Aggregates supported:

- `first`
- `last`
- `sum`
- `max`

Expressions support `+` and `-` over numeric literals and parsed field names.

## Initial wiring

- `src/core/extension/test/parsing.rs` now uses `output_parse` for text fallback parsing in
  `parse_test_results_text()`.
- `src/commands/test.rs` falls back from sidecar JSON to parsed stdout via this primitive.

This keeps extension contracts minimal while centralizing normalization/policy in core.
