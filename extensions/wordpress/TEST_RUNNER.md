# WordPress Extension Test Runner

## Overview

The WordPress extension provides a test runner script for testing WordPress plugins and components using PHPUnit. The test runner can be invoked via Homeboy's `test` command or the extension runtime system.

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

### Via Extension Runtime

Run the WordPress extension directly with component context:

```bash
homeboy extension run wordpress --component <component-id> -- --setting database_type=sqlite
```

Example:
```bash
homeboy extension run wordpress --component data-machine -- --setting database_type=sqlite
```

### Direct Execution

Run the test runner script directly with environment variables set:

```bash
export HOMEBOY_EXTENSION_PATH="$HOME/.config/homeboy/extensions/wordpress"
export HOMEBOY_COMPONENT_ID="data-machine"
export HOMEBOY_COMPONENT_PATH="/path/to/component"
export HOMEBOY_SETTINGS_JSON='{"database_type":"sqlite"}'
bash "$HOMEBOY_EXTENSION_PATH/scripts/test-runner.sh"
```

## PHPCS Linting

The test runner uses PHP_CodeSniffer (PHPCS) with WordPress coding standards for linting before running tests.

### Running Lint Manually

```bash
# From extension directory
composer lint

# Or directly with phpcs
./vendor/bin/phpcs --standard=phpcs.xml.dist /path/to/component
```

### Configuration

The `phpcs.xml.dist` file configures PHPCS with:
- **WordPress-Extra** standard (includes WordPress-Core + WordPress-Docs)
- PHP 7.4+ compatibility
- Excluded directories: `vendor/`, `node_extensions/`, `build/`, `dist/`, `tests/`

### Text Domain Auto-Detection

The test runner automatically detects the text domain from the plugin header. No configuration is needed.

**How it works:**
1. Finds the main plugin file (contains `Plugin Name:` header)
2. Extracts the `Text Domain:` value from the plugin header
3. Passes to PHPCS via `--runtime-set text_domain <value>`

**Example plugin header:**
```php
/**
 * Plugin Name: My Plugin
 * Text Domain: my-plugin
 */
```

To verify detection is working:
```bash
HOMEBOY_DEBUG=1 homeboy test my-component
# Output includes: DEBUG: Text domain: my-plugin
```

### Centralized Infrastructure

**Important:** Components should NOT have local `phpcs.xml.dist` files. The WordPress extension provides centralized PHPCS infrastructure to ensure consistent standards across all components.

### Rules Configuration

**Excluded Rules (PSR-4 Compatibility):**
- `WordPress.Files.FileName.NotHyphenatedLowercase` - PSR-4 uses PascalCase.php
- `WordPress.Files.FileName.InvalidClassFileName` - PSR-4 class naming
- `WordPress.NamingConventions.ValidFunctionName.MethodNameInvalid` - PSR-4 uses camelCase methods
- `WordPress.NamingConventions.ValidVariableName.*` - PSR-4 uses camelCase variables

**Enforced Rules (Security/Best Practices):**
- `WordPress.PHP.YodaConditions` - Prevents accidental assignment
- `WordPress.DB.PreparedSQL` - SQL injection prevention
- `WordPress.Security.*` - XSS prevention
- `WordPress.WP.AlternativeFunctions` - Proper WordPress API usage
- `WordPress.WP.I18n` - Internationalization (text domain auto-detected)
- `WordPress.PHP.StrictInArray` - Type safety

### Linting Behavior

- **Project-level**: Lints all PHP files in the project directory
- **Component-level**: Lints only PHP files in the component directory
- Lint failures abort the test run with exit code 1

## ESLint (JavaScript)

The test runner uses ESLint with WordPress JavaScript coding standards for linting JavaScript files before running tests.

### Automatic Detection

ESLint only runs when JavaScript files exist in the component. The test runner automatically detects JS/JSX/TS/TSX files (excluding node_extensions, vendor, build, dist, and minified files).

### Running Lint Manually

```bash
# From extension directory
npm run lint:js /path/to/component

# Or directly with eslint
./node_extensions/.bin/eslint --config .eslintrc.json --ext .js,.jsx,.ts,.tsx /path/to/component
```

### Configuration

The `.eslintrc.json` file configures ESLint with:
- **@wordpress/eslint-plugin/recommended** standard
- Browser and ES2021 environment
- WordPress globals (`wp`, `jQuery`, `ajaxurl`)
- Excluded directories: `node_extensions/`, `vendor/`, `build/`, `dist/`, `tests/`, `*.min.js`

### Text Domain Auto-Detection

Same as PHPCS - the test runner automatically detects the text domain from the plugin header and passes it to ESLint's `@wordpress/i18n-text-domain` rule.

To verify detection is working:
```bash
HOMEBOY_DEBUG=1 homeboy test my-component
# Output includes: DEBUG: Text domain: my-plugin
```

### Centralized Infrastructure

**Important:** Components should NOT have local `.eslintrc` or `.eslintrc.json` files. The WordPress extension provides centralized ESLint infrastructure to ensure consistent standards across all components.

### Rules Configuration

**Enforced Rules:**
- `@wordpress/dependency-group` - Import grouping (error)
- `@wordpress/i18n-translator-comments` - Translator comments (warn)
- `@wordpress/no-unsafe-wp-apis` - Unsafe API usage (warn)
- `no-console` - Console statements (warn)

**Disabled Rules:**
- `import/no-extraneous-dependencies` - Allows flexible dependency management

### ESLint Behavior

- **Presence-based**: Only runs if JS/JSX/TS/TSX files exist in the component
- **Non-blocking on missing**: If npm dependencies aren't installed, warns and continues
- ESLint failures abort the test run with exit code 1

## Debug Mode

Enable debug output to see environment variables and execution context:

```bash
HOMEBOY_DEBUG=1 homeboy test data-machine
```

When `HOMEBOY_DEBUG=1`, the test runner will display:
- Environment variables being passed
- Execution context (component vs project)
- Extension and component paths
- Database configuration

Without `HOMEBOY_DEBUG=1`, the test runner runs silently except for PHPUnit output.

## Streaming Behavior

- When run interactively (TTY detected), test output streams directly to your terminal
- When run non-interactively (piped output), output is captured and returned as JSON
- The homeboy binary exits with the same exit code as the child process (PHPUnit)

## Local Test Infrastructure

**IMPORTANT:** Components should NOT have local test infrastructure files:
- `tests/bootstrap.php`
- `phpunit.xml`

The WordPress extension provides complete testing infrastructure including WordPress environment setup, database configuration, PHPUnit configuration, and test discovery. Local test infrastructure files will cause the test runner to fail with a clear error message.

### Removing Local Infrastructure

If your component has these files, remove them:

```bash
rm tests/bootstrap.php
rm phpunit.xml
```

Your test files remain - only infrastructure files are removed.

### Build-Time Overrides

**Skip tests during build:**
```bash
HOMEBOY_SKIP_TESTS=1 homeboy build my-component
```

**Use local test infrastructure (rare, requires full WP setup):**
```bash
HOMEBOY_USE_LOCAL_TESTS=1 homeboy build my-component
```

When using local test infrastructure, you must have:
- PHPUnit installed (via `composer install`)
- WordPress test suite (wp-phpunit/wp-phpunit)
- PHPUnit Polyfills (yoast/phpunit-polyfills)
- Database configuration
- Complete bootstrap setup

## Database Options

The test runner supports both SQLite and MySQL databases:

### SQLite (Default)

```bash
homeboy test data-machine
# or
homeboy extension run wordpress --component data-machine -- --setting database_type=sqlite
```

### MySQL

```bash
homeboy extension run wordpress --component data-machine \
  -- --setting database_type=mysql \
     --setting mysql_host=localhost \
     --setting mysql_database=wordpress_test \
     --setting mysql_user=root \
     --setting mysql_password=
```

## Component Configuration

Components must have WordPress extension configured in their component config:

```json
{
  "extensions": {
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
