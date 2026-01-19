# Lint Command

Lint a component using its configured module's linting infrastructure.

## Synopsis

```bash
homeboy lint <component> [options]
```

## Description

The `lint` command runs code style validation for a component using the linting tools provided by its configured module. For WordPress components, this uses PHPCS (PHP CodeSniffer) with WordPress coding standards.

## Arguments

- `<component>`: Name of the component to lint

## Options

- `--fix`: Auto-fix formatting issues before validating (uses PHPCBF for WordPress)
- `--file <path>`: Lint only a single file (path relative to component root)
- `--glob <pattern>`: Lint only files matching glob pattern (e.g., "inc/**/*.php")
- `--changed-only`: Lint only PHP files modified in the working tree (staged, unstaged, untracked)
- `--errors-only`: Show only errors, suppress warnings
- `--summary`: Show compact summary instead of full output
- `--setting <key=value>`: Override module settings (can be used multiple times)

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

## Module Requirements

For a component to be lintable, it must have:

- A module configured (e.g., `wordpress`)
- The module must provide `scripts/lint-runner.sh`

## Environment Variables

The following environment variables are set for lint runners:

- `HOMEBOY_MODULE_PATH`: Absolute path to module directory
- `HOMEBOY_COMPONENT_PATH`: Absolute path to component directory
- `HOMEBOY_PLUGIN_PATH`: Same as component path
- `HOMEBOY_AUTO_FIX`: Set to `1` when `--fix` flag is used
- `HOMEBOY_SUMMARY_MODE`: Set to `1` when `--summary` flag is used
- `HOMEBOY_LINT_FILE`: Single file path when `--file` is used
- `HOMEBOY_LINT_GLOB`: Glob pattern when `--glob` or `--changed-only` is used
- `HOMEBOY_ERRORS_ONLY`: Set to `1` when `--errors-only` flag is used
- `HOMEBOY_SETTINGS_JSON`: Merged settings as JSON string

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

## Exit Codes

- `0`: Linting passed
- `1`: Linting failed (style violations found)
- `2`: Infrastructure error (component not found, missing module, etc.)

## Related

- [test](test.md) - Run tests (includes linting by default)
- [build](build.md) - Build a component (runs pre-build validation)
