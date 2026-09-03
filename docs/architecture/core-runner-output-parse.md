# Core runner + output parse substrate

This document defines the core primitives introduced for:

- #460 — extension runner helper contract
- #464 — generic output parsing primitive

## Runner contract (core)

`crates/contracts/homeboy-extension-contract/src/runner_contract.rs`

- `RunnerStepFilter { step, skip }`
- `should_run(step_name)` for deterministic include/skip semantics
- `to_env_pairs()` maps to exec context vars:
  - `HOMEBOY_STEP`
  - `HOMEBOY_SKIP`

`execution.rs` now aliases legacy `ExtensionStepFilter` to `RunnerStepFilter` to keep command API
stable while moving behavior to a reusable core primitive.

## Output parse primitive (core)

`crates/homeboy-engine-primitives/src/output_parse.rs` (re-exported from `homeboy-core` as `crate::engine::output_parse`)

Generic parser with declarative rule spec:

- `ParseRule { pattern, field, group, aggregate }`
- `DeriveRule { field, expr }`
- `ParseSpec { extension_script, adapters, rules, defaults, derive }`
- `ParseSpec::parse(&self, text) -> HashMap<String, f64>` is the public entry
  point. The free function `parse_output(text, spec)` behind it is private to
  the module.

Aggregates supported:

- `first`
- `last`
- `sum`
- `max`

Expressions support `+` and `-` over numeric literals and parsed field names.

## Initial wiring

- `crates/homeboy-core/src/extension/test/parsing.rs` uses `output_parse` for text fallback parsing in
  `parse_test_results_text()` / `parse_test_results_text_with_spec()`.
- `crates/homeboy-cli/src/commands/test.rs` falls back from sidecar JSON to parsed stdout via this primitive.

This keeps extension contracts minimal while centralizing normalization/policy in core.
