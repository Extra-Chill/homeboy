# Verification phase contract

Homeboy's verification surface is deliberately split into isolated primitive
commands that can be composed by higher-level workflows.

## Primitive phases

The shared phase vocabulary is:

1. `syntax` — parser-level validation such as `php -l`, `rustc --emit=metadata`, or equivalent
2. `lint` — static lint findings from PHPCS, ESLint, clippy, rustfmt, etc.
3. `typecheck` — type/system checks such as PHPStan, `tsc --noEmit`, or `cargo check`
4. `audit` — Homeboy structural analysis such as duplicates, orphaned tests, and code smells
5. `test` — behavioral test harness execution such as PHPUnit, cargo test, or npm test

These phases are a contract, not a requirement that one command runs all of
them. `homeboy test` runs only the `test` phase. `homeboy lint` runs only the
`lint` phase. `homeboy audit` runs only the `audit` phase. A composed command
or CI wrapper can run the phases in canonical order when it needs a full check.

## Exit codes

Every primitive command uses the same exit code convention:

- `0`: clean
- `1`: findings, lint violations, audit findings, or test failures
- `2` or higher: infrastructure failure such as missing dependencies, harness bootstrap failure, or runtime crash

This distinction matters because code findings should fail CI differently from
broken tooling. Composed workflows can stop early on infrastructure failures and
still report normal findings as actionable code work.

## Structured output

Primitive command output should include a phase report:

```json
{
  "phase": {
    "phase": "test",
    "status": "failed",
    "exit_code": 1,
    "summary": "test phase reported 2 failure(s) out of 120 test(s)"
  },
  "failure": {
    "phase": "test",
    "category": "findings",
    "summary": "2 test failure(s) detected"
  }
}
```

`status` is one of:

- `passed`
- `failed`
- `error`
- `skipped`
- `not-run`

`failure.category` is one of:

- `findings`
- `infrastructure`

The core Rust contract lives in `src/core/extension/runner_contract.rs`:

- `VerificationPhase`
- `PhaseStatus`
- `PhaseFailureCategory`
- `PhaseReport`
- `PhaseFailure`

## Composition

Composed workflows should treat primitive commands as independent inputs:

```text
syntax     -> optional primitive / extension runner
lint       -> homeboy lint
typecheck  -> optional primitive / extension runner
audit      -> homeboy audit
test       -> homeboy test
```

This keeps each command debuggable on its own while giving CI, future `check`
commands, and rig workflows one stable shape to aggregate.
