# Lint Command

Lint a component using its configured extension's linting infrastructure.

## Synopsis

```bash
homeboy lint <component> [options]
```

## Description

The `lint` command runs code style validation for a component using the linting tools provided by its configured extension. For WordPress components, this uses PHPCS (PHP CodeSniffer) with WordPress coding standards.

## Arguments

- `<component>`: Name of the component to lint

## Options

- `--fix`: Auto-fix formatting issues before validating (uses PHPCBF for WordPress)
- `--baseline`: Save current lint findings as baseline for future comparisons
- `--ignore-baseline`: Skip baseline comparison even if baseline exists
- `--file <path>`: Lint only a single file (path relative to component root)
- `--glob <pattern>`: Lint only files matching glob pattern (e.g., "inc/**/*.php")
- `--changed-only`: Lint only files modified in the working tree (staged, unstaged, untracked)
- `--errors-only`: Show only errors, suppress warnings
- `--summary`: Show compact summary instead of full output
- `--setting <key=value>`: Override extension settings (can be used multiple times)

## Examples

```bash
# Lint a WordPress component
homeboy lint extrachill-api

# Auto-fix formatting issues then validate
homeboy lint extrachill-api --fix

# Lint only modified files in the working tree
homeboy lint extrachill-api --changed-only

# Lint only a single file
homeboy lint extrachill-api --file inc/core/api.php

# Lint files matching a glob pattern
homeboy lint extrachill-api --glob "inc/**/*.php"

# Lint with custom settings
homeboy lint extrachill-api --setting some_option=value
```

## Extension Requirements

For a component to be lintable, it must have:

- A extension configured (e.g., `wordpress`)
- The extension must provide a lint-runner script (at scripts/lint-runner.sh within the extension)

## Environment Variables

The following environment variables are set for lint runners:

- `HOMEBOY_MODULE_PATH`: Absolute path to extension directory
- `HOMEBOY_COMPONENT_PATH`: Absolute path to component directory
- `HOMEBOY_PLUGIN_PATH`: Same as component path
- `HOMEBOY_AUTO_FIX`: Set to `1` when `--fix` flag is used
- `HOMEBOY_SUMMARY_MODE`: Set to `1` when `--summary` flag is used
- `HOMEBOY_LINT_FILE`: Single file path when `--file` is used
- `HOMEBOY_LINT_GLOB`: Glob pattern when `--glob` or `--changed-only` is used
- `HOMEBOY_ERRORS_ONLY`: Set to `1` when `--errors-only` flag is used
- `HOMEBOY_SETTINGS_JSON`: Merged settings as JSON string
- `HOMEBOY_LINT_FINDINGS_FILE`: Path for extension to write structured lint findings JSON

## Output

Returns JSON with lint results:

```json
{
  "status": "passed|failed",
  "component": "component-name",
  "output": "lint output...",
  "exit_code": 0,
  "hints": ["Run 'homeboy lint <component> --fix' to auto-fix..."]
}
```

The `hints` field appears when linting fails without `--fix`, suggesting the auto-fix option.

When extensions write `HOMEBOY_LINT_FINDINGS_FILE`, Homeboy exposes `lint_findings` in JSON output and
supports baseline ratchet checks (`--baseline`, `--ignore-baseline`).

## Exit Codes

- `0`: Linting passed
- `1`: Linting failed (style violations found)
- `2`: Infrastructure error (component not found, missing extension, etc.)

## Related

- [test](test.md) - Run tests (includes linting by default)
- [build](build.md) - Build a component (runs pre-build validation)
