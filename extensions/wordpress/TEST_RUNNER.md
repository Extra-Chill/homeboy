# WordPress Module Test Runner

## Overview

The WordPress module provides a test runner script for testing WordPress plugins and components using PHPUnit. The test runner can be invoked via Homeboy's `test` command or the module runtime system.

## Running Tests

### Via Homeboy Test Command

Run tests for a component using Homeboy's test command:

```bash
homeboy test <component-id>
```

Example:
```bash
homeboy test data-machine
```

### Via Module Runtime

Run the WordPress module directly with component context:

```bash
homeboy module run wordpress --component <component-id> -- --setting database_type=sqlite
```

Example:
```bash
homeboy module run wordpress --component data-machine -- --setting database_type=sqlite
```

### Direct Execution

Run the test runner script directly with environment variables set:

```bash
export HOMEBOY_MODULE_PATH="$HOME/.config/homeboy/modules/wordpress"
export HOMEBOY_COMPONENT_ID="data-machine"
export HOMEBOY_COMPONENT_PATH="/path/to/component"
export HOMEBOY_SETTINGS_JSON='{"database_type":"sqlite"}'
bash "$HOMEBOY_MODULE_PATH/scripts/test-runner.sh"
```

## Debug Mode

Enable debug output to see environment variables and execution context:

```bash
HOMEBOY_DEBUG=1 homeboy test data-machine
```

When `HOMEBOY_DEBUG=1`, the test runner will display:
- Environment variables being passed
- Execution context (component vs project)
- Module and component paths
- Database configuration

Without `HOMEBOY_DEBUG=1`, the test runner runs silently except for PHPUnit output.

## Streaming Behavior

- When run interactively (TTY detected), test output streams directly to your terminal
- When run non-interactively (piped output), output is captured and returned as JSON
- The homeboy binary exits with the same exit code as the child process (PHPUnit)

## Database Options

The test runner supports both SQLite and MySQL databases:

### SQLite (Default)

```bash
homeboy test data-machine
# or
homeboy module run wordpress --component data-machine -- --setting database_type=sqlite
```

### MySQL

```bash
homeboy module run wordpress --component data-machine \
  -- --setting database_type=mysql \
     --setting mysql_host=localhost \
     --setting mysql_database=wordpress_test \
     --setting mysql_user=root \
     --setting mysql_password=
```

## Component Configuration

Components must have WordPress module configured in their component config:

```json
{
  "modules": {
    "wordpress": {
      "settings": {
        "database_type": "sqlite"
      }
    }
  }
}
```

## Test Requirements

The component must have a `tests/` directory with PHPUnit test files. The test runner will automatically discover and run tests in this directory.
