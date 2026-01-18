# Test Command

Run test suites for Homeboy components/modules.

## Synopsis

```bash
homeboy test <component> [options]
```

## Description

The `test` command executes test suites for specified Homeboy components. It automatically discovers and runs the appropriate test infrastructure for each component type.

## Arguments

- `<component>`: Name of the component/module to test (must exist in `homeboy-modules/`)

## Options

- `--project-path <path>`: Path to the project directory containing tests (defaults to current directory)
- `--setting <key=value>`: Override component settings (can be used multiple times)

## Examples

```bash
# Test the wordpress component with default settings
homeboy test wordpress

# Test wordpress with MySQL instead of SQLite
homeboy test wordpress --setting database_type=mysql --setting mysql_host=localhost

# Test a component in a specific project directory
homeboy test wordpress --project-path /path/to/wordpress/project

# Test with multiple setting overrides
homeboy test wordpress --setting database_type=mysql --setting mysql_database=test_db
```

## Component Requirements

For a component to be testable, it must have:

- A directory in `homeboy-modules/{component}/`
- A `{component}.json` manifest file
- A `scripts/test-runner.sh` executable script

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
- `HOMEBOY_MODULE_PATH`: Absolute path to module directory
- `HOMEBOY_PROJECT_PATH`: Absolute path to project directory
- `HOMEBOY_SETTINGS_JSON`: Merged settings as JSON string

## Notes

- Tests run in the component's environment, not the project's
- SQLite provides fastest in-memory testing
- MySQL testing requires a running MySQL server
- Component settings can be configured globally via `homeboy config`