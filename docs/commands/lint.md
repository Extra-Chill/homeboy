# Lint Command

Lint a component using its linked extension's linting infrastructure.

## Synopsis

```bash
homeboy lint [component] [options]
```

## Description

The `lint` command resolves the component's linked extension with `lint` capability and runs that extension's lint runner. Homeboy owns scoping flags, baseline / ratchet behavior, and structured output; the extension owns the language-specific tools.

## Arguments

- `[component]`: Component ID. Optional when Homeboy can auto-detect a portable `homeboy.json` or registered component from the current directory.

## Options

- `--baseline`: Save current lint findings as baseline for future comparisons
- `--ignore-baseline`: Skip baseline comparison even if baseline exists
- `--path <PATH>`: Override component `local_path` for this run
- `--file <path>`: Lint only a single file (path relative to component root)
- `--glob <pattern>`: Lint only files matching glob pattern (e.g., "inc/**/*.php")
- `--changed-only`: Lint only files modified in the working tree (staged, unstaged, untracked)
- `--changed-since <REF>`: Lint only files changed since a git ref
- `--errors-only`: Show only errors, suppress warnings
- `--summary`: Show compact summary instead of full output
- `--sniffs <SNIFFS>`: Restrict to comma-separated linter sniffs or rules when supported
- `--exclude-sniffs <SNIFFS>`: Exclude comma-separated linter sniffs or rules when supported
- `--category <CATEGORY>`: Restrict to a named linter category when supported
- `--fix`: Apply auto-fixable lint findings in place. Thin alias for `homeboy refactor <component> --from lint --write` — dispatches to the existing fixer pipeline so a single ergonomic flag resolves the auto-fix CTA.
- `--setting <key=value>`: Override extension settings (can be used multiple times)
- `--setting-json <key=json>`: Override extension settings with typed JSON values

## Examples

```bash
# Lint a WordPress component
homeboy lint extrachill-api

# Auto-fix formatting issues (ergonomic alias)
homeboy lint extrachill-api --fix

# Auto-fix formatting issues (canonical invocation, identical behavior)
homeboy refactor extrachill-api --from lint --write

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

- A linked extension that declares a `lint` capability
- A lint runner declared by the extension's `lint.extension_script`

## Environment Variables

The following environment variables are set for lint runners:

- `HOMEBOY_EXTENSION_ID`: Extension identifier
- `HOMEBOY_EXTENSION_PATH`: Absolute path to extension directory
- `HOMEBOY_COMPONENT_PATH`: Absolute path to component directory
- `HOMEBOY_FIX_ONLY`: Set to `1` when running in fix-only mode (refactor --from lint --write) — extension should apply fixes without re-running diagnostics
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
  "hints": ["Auto-fix: homeboy lint <component> --fix (or homeboy refactor <component> --from lint --write)"]
}
```

The `hints` field appears when linting fails, suggesting the auto-fix CTA. Per the
contract under [#1507](https://github.com/Extra-Chill/homeboy/issues/1507),
autofixable findings never fail the run on their own — they nudge the user to
run `homeboy lint --fix`.

When extensions write `HOMEBOY_LINT_FINDINGS_FILE`, Homeboy exposes `lint_findings` in JSON output and
supports baseline ratchet checks (`--baseline`, `--ignore-baseline`).

## Exit Codes

- `0`: Linting passed
- `1`: Linting failed (style violations found)
- `2`: Infrastructure error (component not found, missing extension, etc.)

## Related

- [test](test.md) - Run tests (includes linting by default)
- [build](build.md) - Build a component (runs pre-build validation)
