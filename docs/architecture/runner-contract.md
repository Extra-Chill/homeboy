# Runner contract

The contract between Homeboy core and runner scripts: what capabilities
exist, what env vars flow in, what sidecar files scripts are expected to
write, and what exit codes mean.

This is the authoritative reference for extension authors wiring a new
runner and for core maintainers improving the cross-extension surface.

For the cross-command verification phase model (`syntax`, `lint`, `typecheck`,
`audit`, `test`), see
[`docs/development/contracts/verification-phases.md`](../development/contracts/verification-phases.md).

## Capability model

Each extension can declare scripts per-capability in its manifest
(`<extension-id>.json`). Components can also declare self-hosted scripts
directly in `homeboy.json` under `scripts.<capability>`. Component scripts
resolve first, linked extension behavior resolves second, and missing support
is not-applicable.

Four capabilities are first-class in core:

| Capability | Manifest field | Typical script | Invoked by |
|------------|---------------|----------------|------------|
| `lint` | `lint.extension_script` | extension-owned lint runner | `homeboy lint` |
| `test` | `test.extension_script` | extension-owned test runner | `homeboy test` |
| `build` | `build.extension_script` | extension-owned build runner | `homeboy build`, `homeboy release` |
| `audit` | *built-in to core* | n/a | `homeboy audit` |

`lint`, `test`, and `build` are shell-script capabilities: extensions own
the runtime. `audit` is a core-owned framework (pattern detectors, shared
scaffolding checks, orphaned-test detection, etc.) — extensions don't
implement it directly.

Component-owned scripts use the same capability contract without extension
claiming. They run sequentially in the component root and set
`HOMEBOY_EXTENSION_ID=component-script` plus `HOMEBOY_EXTENSION_PATH` to the
component path so existing runner helpers can identify the source.

Extensions may omit any capability. Detection uses `has_lint()` /
`has_test()` / `has_build()` accessors on the manifest (see
`src/core/extension/manifest.rs`). If a capability is missing, the
corresponding homeboy command exits cleanly with a "not applicable"
message rather than failing.

## Step filtering

Within a capability, extensions often run multiple tools (e.g. `lint`
runs PHPCS, PHPStan, and ESLint). Step-level filtering is a shared core
primitive:

- `HOMEBOY_STEP=phpcs,eslint` — only listed steps run
- `HOMEBOY_SKIP=phpstan` — listed steps are skipped
- Both empty — every step runs

Extensions source `runner-steps.sh` (injected by core) and call
`should_run_step "<name>"` at each gate:

```bash
if ! should_run_step "phpcs"; then
    echo "Skipping PHPCS (step filter)"
else
    # ... run phpcs
fi
```

Step names are extension-chosen; core only enforces the filter
semantics. The contract type lives at
`src/core/extension/runner_contract.rs` (`RunnerStepFilter`) and
serializes to the env pair above.

## Environment inputs

Every extension script receives the base execution context (see
[execution-context.md](execution-context.md) for the full list). The
variables most runners care about:

| Variable | Source | Meaning |
|----------|--------|---------|
| `HOMEBOY_EXTENSION_PATH` | core | Absolute path to the extension's install dir |
| `HOMEBOY_COMPONENT_ID` | core (when in component scope) | Component identifier |
| `HOMEBOY_COMPONENT_PATH` | core (when in component scope) | Absolute path to the component |
| `HOMEBOY_PROJECT_PATH` | core (when in project scope) | Absolute path to project root |
| `HOMEBOY_SETTINGS_JSON` | core | JSON blob of merged settings |
| `HOMEBOY_STEP` / `HOMEBOY_SKIP` | core | CSV step filter (see above) |
| `HOMEBOY_FIX_ONLY` | `homeboy refactor --from lint --write` | `"1"` → run fixers, skip validation |
| `HOMEBOY_DEBUG` | user | `"1"` → verbose runner output |
| `HOMEBOY_RUNTIME_*` | core | Paths to core-provided runtime helpers (see below) |

Trace runners also receive trace-specific variables when invoked by `homeboy trace`:

| Variable | Source | Meaning |
|----------|--------|---------|
| `HOMEBOY_TRACE_RESULTS_FILE` | core | JSON trace envelope path the runner should write |
| `HOMEBOY_TRACE_SCENARIO` | CLI | Scenario ID being executed |
| `HOMEBOY_TRACE_LIST_ONLY` | core | `"1"` when listing scenarios instead of running one |
| `HOMEBOY_TRACE_ARTIFACT_DIR` | core | Directory for runner artifacts |
| `HOMEBOY_TRACE_ATTACHMENTS` | CLI | JSON array of observation-only attach targets from repeatable `--attach KIND:TARGET` |

`HOMEBOY_TRACE_ATTACHMENTS` v1 supports local `logfile`, `fswatch`, `pid`, `port`, `http`, and `systemd` targets. HTTP attachments accept `http:<url>` or a direct `http://` / `https://` URL. Core observes attachments before and after the scenario and writes timeline events plus an attachment observation artifact in the run directory; runners may also read the same JSON to correlate their own scenario events. `fswatch:<path>` keeps the safe file metadata snapshots and also starts the same passive polling `file.watch` probe used by rig workloads, deduplicated against explicit rig probes for the same path. `systemd:<unit>` reads local `systemctl show` state and main PID metadata when available. Attachments are explicitly observation-only: runners and core must not start, stop, restart, or kill attached targets as part of attach handling.

## Core-provided runtime helpers

Core ships shell helpers as embedded assets
(`src/core/extension/runtime/`) and injects their absolute paths via
`HOMEBOY_RUNTIME_*` env vars. Extensions source them at the top of the
runner script with a fallback to a bundled copy:

### `runner-prelude.sh` (env: `HOMEBOY_RUNTIME_RUNNER_PRELUDE`)

Provides `homeboy_runner_init`, `homeboy_source_runtime_helper`, and the
shared bash-version guard for runners that want the standard context,
step-filter, sidecar-writer, and failure-trap setup in one source call.

### `bash-preflight.sh` (env: `HOMEBOY_RUNTIME_BASH_PREFLIGHT`)

Provides `homeboy_require_bash_version <major>` for scripts that only need
the bash-version guard and do not need the full runner prelude.

### `runner-steps.sh` (env: `HOMEBOY_RUNTIME_RUNNER_STEPS`)

Provides `should_run_step <name>` for the step-filter semantics described
above. See the helper source for the exact contract. Required if the
runner has multiple internal tools.

### `failure-trap.sh` (env: `HOMEBOY_RUNTIME_FAILURE_TRAP`)

Provides `homeboy_init_failure_trap` which registers an EXIT trap that
prints a standard banner when a step fails. Extensions set three
variables to control it:

- `FAILED_STEP` — name of the failing step (required)
- `FAILURE_OUTPUT` — captured error output for replay (optional)
- `FAILURE_REPLAY_MODE` — `"full"` (default) or `"none"`

The banner looks like:

```
============================================
BUILD FAILED: <step-name>
============================================

Error details:
<captured output, if any>
```

Extensions using this helper get consistent failure presentation for free
across the ecosystem.

### `write-test-results.sh` (env: `HOMEBOY_RUNTIME_WRITE_TEST_RESULTS`)

Provides `homeboy_write_test_results <total> <passed> <failed> <skipped>
[partial_label]` which writes the canonical test-results JSON sidecar
(see next section).

## Sidecar output contracts

Extensions write structured results to paths in env vars. Core reads
the files back and parses them into the structured CLI response. Writing
the sidecar is optional — core falls back to text parsing — but writing
it makes results reliable across tool versions.

### `HOMEBOY_TEST_RESULTS_FILE` — test counts

Standard shape (see `write-test-results.sh`):

```json
{
  "total": 42,
  "passed": 41,
  "failed": 1,
  "skipped": 0
}
```

Optional `"partial": "<label>"` field when counts are incomplete (e.g.
`"testdox-fallback"` when only a summary line is parseable).

### `HOMEBOY_TEST_FAILURES_FILE` — failure details

Array of per-failure objects with file, line, test name, and the error
message. Used by `homeboy test --analyze` for cluster analysis.

### `HOMEBOY_LINT_FINDINGS_FILE` — lint findings

Array of objects with the shape:

```json
[
  {
    "id": "path/to/file.php::WordPress.Security.EscapeOutput::42",
    "message": "All output should be run through an escaping function (WordPress.Security.EscapeOutput)",
    "category": "security"
  }
]
```

`id` is an identity key for the baseline ratchet — stable across runs
when the finding is unchanged. `category` is derived from the tool's
rule namespace (see the WordPress extension's `lint-runner.sh` for the
canonical category mapping).

### `HOMEBOY_COVERAGE_FILE` — coverage report

Emitted when `homeboy test --coverage` is passed. Tool-specific; core
parses it via `parse_coverage_file()` with per-tool handlers.

### `HOMEBOY_ANNOTATIONS_DIR` — CI inline annotations

Directory path where extensions drop per-tool JSON (`phpcs.json`,
`phpstan.json`, `eslint.json`) describing findings in a format suitable
for GitHub CI inline comments. Each file is an array of
`{file, line, message, source, severity, code, fixable}` entries.

### `HOMEBOY_FIX_RESULTS_FILE` / `HOMEBOY_FIX_PLAN_FILE`

Emitted in fix-only mode (`HOMEBOY_FIX_ONLY=1`). Array of
`{file, rule, action, confidence}` entries describing what the fixer
did (or would do, in plan mode). Confidence tiers: `safe`, `guarded`,
`advisory`.

## Exit codes

**Core's convention** for runner scripts:

- `0` — clean. No findings, tests all passed, etc.
- `1` — findings or failures in this run (normal "something to fix" case).
- `2` or higher — infrastructure failure (missing dependency, runtime
  crash, bootstrap failure before the real work started).

Extensions MUST distinguish `1` from `≥2` to give core the information
it needs to surface genuine infrastructure problems rather than showing
them as "test failures."

### Existing classifiers

The wordpress Playground runner is the most thorough example
(see `homeboy-extensions:wordpress/scripts/test/test-runner-playground.sh`
classification block, lines 282–374). It distinguishes 8 failure modes:

1. Bootstrap failure with captured stage (e.g. "install stage failed")
2. PHPUnit assertion failures ("SOME TESTS FAILED")
3. PHPUnit fatal on stdout (FAILURES/ERRORS pattern)
4. PHP parse/fatal before runner took control
5. Unclassified non-zero exit
6. No output captured at all
7. Discovery found zero test files
8. Zero tests executed (class didn't extend `TestCase`, etc.)

Each produces a distinct `FAILED_STEP` label and either dumps
diagnostics or replays the tool output.

**Consolidation target:** factor this classifier into a future shared
runtime helper under `src/core/extension/runtime/` (tracked in
[Extra-Chill/homeboy#1459](https://github.com/Extra-Chill/homeboy/issues/1459))
so rust, swift, and future extensions produce the same categorized
surface without re-implementing the logic. The helper does not exist
yet — this is a follow-up deliverable, not a current reference.

## Command-level behavior

### `homeboy test`

Invokes the extension's `test.extension_script` with context env vars
set. The script is expected to:

1. Run the test harness only (PHPUnit, cargo test, npm test, etc.).
2. Write results sidecar if `HOMEBOY_TEST_RESULTS_FILE` is set.
3. Write failures sidecar if `HOMEBOY_TEST_FAILURES_FILE` is set.
4. Exit per the convention above.

`homeboy test` does not run lint or audit. Those are separate primitive
commands (`homeboy lint`, `homeboy audit`) that composed workflows can run
alongside test when they need a full verification sequence.

Core handles baseline comparison, coverage threshold enforcement,
test-drift detection, and analysis mode — extensions don't implement
those features themselves.

### `homeboy lint`

Invokes `lint.extension_script` directly. Supports step filtering
(`--step phpcs`, `--skip phpstan`) via the env pairs above. In fix-only
mode (`homeboy refactor --from lint --write`), sets
`HOMEBOY_FIX_ONLY=1` which signals the runner to run fixers and skip
validation.

### `homeboy build`

Invokes `build.extension_script`. Sidecar contracts are different (build
artifacts, version targets) — see [release-pipeline.md](release-pipeline.md).

### `homeboy trace`

Invokes `trace.extension_script` with the trace-specific sidecar and artifact variables documented above. The runner drives the requested scenario and writes a trace results envelope to `HOMEBOY_TRACE_RESULTS_FILE`.

When `--attach` is present, core observes the declared already-running local targets before and after the runner executes. `fswatch` attachments also collect passive `file.watch` timeline events during the run. This augments the trace evidence but does not replace the scenario: the extension script still runs normally, and attach handling does not own the target lifecycle.

### `homeboy audit`

Runs entirely in core. No extension script invoked. Audit rules read
the component's manifest for configuration
(`audit.feature_patterns`, `audit.test_mapping`, etc.) but the detectors
themselves live in `src/core/code_audit/`.

## Authoring a new runner

Minimum viable runner for a new extension capability:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Source core helpers with fallback
RUNNER_STEPS="${HOMEBOY_RUNTIME_RUNNER_STEPS:-$(dirname "$0")/lib/runner-steps.sh}"
FAILURE_TRAP="${HOMEBOY_RUNTIME_FAILURE_TRAP:-$(dirname "$0")/lib/failure-trap.sh}"
# shellcheck source=/dev/null
[ -f "$RUNNER_STEPS" ] && source "$RUNNER_STEPS"
# shellcheck source=/dev/null
[ -f "$FAILURE_TRAP" ] && source "$FAILURE_TRAP"

homeboy_init_failure_trap

# Run tool-1 if step filter allows
if should_run_step "tool-1"; then
    if ! run_tool_1; then
        FAILED_STEP="tool-1"
        exit 1
    fi
fi

# Run tool-2 if step filter allows
if should_run_step "tool-2"; then
    if ! run_tool_2; then
        FAILED_STEP="tool-2"
        exit 1
    fi
fi

exit 0
```

Write sidecar output when requested:

```bash
if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ]; then
    source "${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS}"
    homeboy_write_test_results "$total" "$passed" "$failed" "$skipped"
fi
```

## Related

- [execution-context.md](execution-context.md) — full env var list and template-variable resolution.
- [core-runner-output-parse.md](core-runner-output-parse.md) — generic output parsing primitive for text fallback.
- [output-system.md](output-system.md) — JSON envelope wrapping runner results in CLI responses.
- [hooks.md](hooks.md) — pre/post hooks around capability execution.
