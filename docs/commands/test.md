# Test Command

Run a component's extension-backed test suite.

## Synopsis

```bash
homeboy test [component] [options] [-- <runner-args>]
```

## Description

The `test` command resolves the component's linked extension with `test` capability, runs its configured test runner, and applies Homeboy's baseline / ratchet handling to structured test output when available.

## Arguments

- `[component]`: Component ID. Optional when Homeboy can auto-detect a portable `homeboy.json` or registered component from the current directory.

## Options

- `--skip-lint`: Skip linting before running tests
- `--coverage`: Collect code coverage when the runner supports it
- `--coverage-min <PERCENT>`: Fail when coverage is below this threshold; implies `--coverage`
- `--baseline`: Persist the current test result baseline
- `--ignore-baseline`: Skip baseline comparison for this run
- `--ratchet`: Auto-update the baseline when the current run improves on it
- `--drift`: Cross-reference production changes with test files
- `--write`: Write fixes to disk for workflows that support it
- `--since <REF>`: Git ref for drift detection (default `HEAD~10`)
- `--setting <key=value>`: Override component settings (can be used multiple times)
- `--setting-json <key=json>`: Override component settings with typed JSON values
- `--path <PATH>`: Override component `local_path` for this run
- `--changed-since <REF>`: Limit execution to impacted tests since a git ref
- `--ci-job <ID>`: Run using env and passthrough args from a single extension-declared CI job whose `command` is `test`
- `--analyze`: Cluster and summarize failures
- `--json-summary`: Include compact structured summary in JSON output for CI wrappers

## Examples

```bash
# Test the current component from a repo with homeboy.json
homeboy test

# Test a registered component with setting overrides
homeboy test my-component --setting database_type=mysql --setting mysql_host=localhost

# Run tests only, skip linting
homeboy test my-component --skip-lint

# Test with multiple setting overrides
homeboy test my-component --setting database_type=mysql --setting mysql_database=test_db

# Reproduce an extension-declared CI test job through the test workflow
homeboy test my-component --ci-job unit
```

## Passthrough Arguments

Arguments after `--` are passed directly to the extension's test runner script:

```bash
# Pass a single argument
homeboy test my-extension -- --filter=SomeTest

# Pass multiple arguments
homeboy test my-extension -- --filter=SomeTest --verbose
```

Supported arguments depend on the underlying test framework.

When `--ci-job <ID>` is used, args declared on that extension-owned CI job are forwarded before explicit CLI passthrough arguments, and job env is passed to the test runner. Homeboy validates that the selected job declares `command: "test"` before running it.

## Component Requirements

For a component to be testable, it must have:

- A linked extension with test support
- A manifest file for the extension
- A test runner declared by the extension's `test.extension_script`

## Settings

Settings are extension-defined. Use `--setting key=value` for string values and `--setting-json key=<json>` when the runner expects typed values such as objects, arrays, booleans, numbers, or null.

## Output

Returns JSON with test results:

```json
{
  "status": "passed|failed|skipped",
  "component": "component-name",
  "output": "test output...",
  "exit_code": 0
}
```

For WordPress components, a successful activation/install smoke run with zero discovered PHPUnit files is reported as `status: "skipped"` with exit code `0`. The phase summary states that activation/install passed, PHPUnit discovery found zero tests, and no PHPUnit assertions ran. This preserves the smoke-script workflow while making the absence of PHPUnit assertions explicit.

If a component is expected to contain PHPUnit tests, set `require_phpunit_tests=true` through the existing settings surface, for example `--setting require_phpunit_tests=true` or the component's extension settings. With that setting enabled, zero PHPUnit discovery is treated as a test failure.

## Exit Codes

- `0`: Tests passed
- `0`: PHPUnit discovery skipped because no tests were found, when not required
- `1`: Tests failed
- `2`: Infrastructure error (component not found, missing scripts, etc.)

## Environment Variables

The following environment variables are set for test runners:

- `HOMEBOY_EXEC_CONTEXT_VERSION`: Protocol version (`"2"`)
- `HOMEBOY_EXTENSION_ID`: Extension identifier
- `HOMEBOY_EXTENSION_PATH`: Absolute path to extension directory
- `HOMEBOY_PROJECT_PATH`: Absolute path to project directory
- `HOMEBOY_COMPONENT_ID`: Component identifier
- `HOMEBOY_COMPONENT_PATH`: Absolute path to component directory
- `HOMEBOY_SETTINGS_JSON`: Merged settings as JSON string
- `HOMEBOY_TEST_RESULTS_FILE`: Path where runners can write structured test results
- `HOMEBOY_TEST_FAILURES_FILE`: Path where runners can write structured failure summaries

## Notes

- Tests run in the component's source directory.
- Component settings can live in `homeboy.json`, component config, or CLI overrides depending on how the component is resolved.
