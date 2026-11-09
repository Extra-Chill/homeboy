# Typed command results retain their cause (#17668)

## Changes

- The shared command-result envelope promotes typed nonzero results into summary and diagnostics before the unnamed-failure fallback. Merge holds retain the reason, expected and observed heads, and check names; Cook terminal results retain their status and stop reason.
- Workflow completion uses the child envelope summary for failures and clears errors when the resolved outcome is succeeded, for both steps and foreach items.
- The table-driven public-boundary regression covers head movement, failed check names, and Cook execution-budget exhaustion. It now parses the actual stdout JSON and asserts both failed and succeeded item outcomes, including absence of an error on the succeeded item.

## Validation

- `cargo test -p homeboy-cli commands::utils::response`: 53 passed.
- `cargo test -p homeboy-core workflow`: 105 passed (includes daemon workflow tests).
- `cargo test -p homeboy-core git:: -- --test-threads=1`: 347 passed. The parallel run had three fixture-related failures; all passed in the serial module run.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

## PR body

Fixes #17668, including the Cook comment. Preserve typed failure causes at the shared envelope boundary and keep succeeded workflow outcomes free of stale process errors. Regression coverage exercises serialized command stdout and workflow foreach outcomes for merge holds and Cook terminal states.

Implemented by Homeboy Cook (Pi, openai/gpt-6.1-sol). Issue, prompt and review by Franklin (Claude Opus via Pi, Roadie).
