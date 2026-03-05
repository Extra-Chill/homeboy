# Test Command

Run test suites for Homeboy components/extensions.

## Synopsis

```bash
homeboy test <component> [options]
```

## Description

The `test` command executes test suites for specified Homeboy components. It automatically discovers and runs the appropriate test infrastructure for each component type.

## Arguments

- `<component>`: Name of the component to test (must have a extension configured)

## Options

- `--skip-lint`: Skip linting before running tests
- `--setting <key=value>`: Override component settings (can be used multiple times)
- `--path <PATH>`: Override component `local_path` for this run
- `--changed-since <REF>`: Limit execution to impacted tests since a git ref
- `--analyze`: Cluster and summarize failures
- `--json-summary`: Include compact structured summary in JSON output for CI wrappers

## Examples

```bash
# Test the wordpress component with default settings
homeboy test wordpress

# Test wordpress with MySQL instead of SQLite
homeboy test wordpress --setting database_type=mysql --setting mysql_host=localhost

# Run tests only, skip linting
homeboy test wordpress --skip-lint

# Test with multiple setting overrides
homeboy test wordpress --setting database_type=mysql --setting mysql_database=test_db
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

## Component Requirements

For a component to be testable, it must have:

- A linked extension with test support
- A manifest file for the extension (e.g., wordpress.json)
- A test-runner script provided by the extension (at scripts/test-runner.sh within the extension)

## Supported Components

Currently supported:

- **wordpress**: PHPUnit-based WordPress testing with SQLite/MySQL support

## Settings

Settings vary by component. For WordPress:

- `database_type`: `"sqlite"` (default) or `"mysql"`
- `mysql_host`: MySQL hostname (default: `"localhost"`)
- `mysql_database`: MySQL database name (default: `"wordpress_test"`)
- `mysql_user`: MySQL username (default: `"root"`)
- `mysql_password`: MySQL password (default: `""`)

## Output

Returns JSON with test results:

```json
{
  "status": "passed|failed",
  "component": "component-name",
  "output": "test output...",
  "exit_code": 0
}
```

## Exit Codes

- `0`: Tests passed
- `1`: Tests failed
- `2`: Infrastructure error (component not found, missing scripts, etc.)

## Environment Variables

The following environment variables are set for test runners:

- `HOMEBOY_EXEC_CONTEXT_VERSION`: Protocol version (`"1"`)
- `HOMEBOY_MODULE_ID`: Component name
- `HOMEBOY_MODULE_PATH`: Absolute path to extension directory
- `HOMEBOY_PROJECT_PATH`: Absolute path to project directory
- `HOMEBOY_COMPONENT_ID`: Component identifier
- `HOMEBOY_COMPONENT_PATH`: Absolute path to component directory
- `HOMEBOY_SETTINGS_JSON`: Merged settings as JSON string

## Notes

- Tests run in the component's environment, not the project's
- SQLite provides fastest in-memory testing
- MySQL testing requires a running MySQL server
- Component settings can be configured globally via `homeboy config`
