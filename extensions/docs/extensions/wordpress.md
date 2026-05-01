# WordPress Extension

## Component Shapes

The WordPress extension supports two component shapes:

- `standalone` / default — WordPress plugins and themes. Tests run through WordPress Playground with the component mounted under `wp-content/plugins/<slug>` or the existing theme/plugin path assumptions.
- `core-dev` — a `wordpress-develop` checkout. Tests, lint, and build dispatch to WordPress core's native tooling instead of mounting the checkout into Playground.

Homeboy core may pass `HOMEBOY_COMPONENT_SHAPE=core-dev` for registered components. For direct script execution and smoke tests, the extension also detects `wordpress-develop` by the marker set `wp-config-sample.php`, `src/wp-includes/version.php`, and `tests/phpunit/`.

The core-dev runner expects WordPress core's own dependencies and config. It installs missing npm/composer dependencies, builds `src/` into `build/`, and runs PHPUnit through core's `vendor/bin/phpunit`. If `wp-tests-config.php` is missing, set `HOMEBOY_WP_TESTS_DB_NAME`, `HOMEBOY_WP_TESTS_DB_USER`, `HOMEBOY_WP_TESTS_DB_PASSWORD`, and optionally `HOMEBOY_WP_TESTS_DB_HOST` so the runner can write it from the sample config.

## Validation dependencies

Some WordPress plugins are intentionally layered on top of other local plugins.
The WordPress extension can load those local dependencies during validation so
PHPStan, the autoload preflight check, and PHPUnit all run with the expected
plugin graph instead of in false isolation.

Configure dependencies in the component's WordPress extension settings:

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "validation_dependencies": "data-machine"
      }
    }
  }
}
```

Supported value shapes:

- single component ID: `data-machine`
- comma-separated list: `data-machine, other-plugin`
- newline-separated list
- JSON-array string: `["data-machine", "other-plugin"]`

Each dependency entry may be either:

- a registered Homeboy component ID
- an absolute path to another local plugin checkout

## Lint findings sidecar

When `HOMEBOY_LINT_FINDINGS_FILE` is set, the WordPress lint runner writes a
JSON array of lint finding records for Homeboy baseline and observation storage.
PHPCS, ESLint, and PHPStan findings are merged into the same sidecar.

The sidecar contract is version 1. Records preserve the original minimal fields
(`id`, `message`, `category`, and `fixable` when known) and include normalized
fields where each tool reports them:

- `id` — stable finding identity using `file::code::line`.
- `file` — component-relative path when the file is inside the component.
- `line` / `column` — 1-based location when reported by the linter.
- `severity` — normalized `error` or `warning`.
- `source` — linter name, such as `phpcs`, `eslint`, or `phpstan`.
- `code` — tool-specific rule, sniff, or identifier.
- `category` — broad grouping used by Homeboy reports.
- `message` — human-readable linter message, including the tool code.
- `fixable` — whether the linter reports an automatic fix for the finding.
- `fingerprint` — stable SHA-1 hash of the finding `id`.
- `excerpt` — source line text when the file is readable locally; otherwise
  `null`.
