# Testing

The WordPress extension runs PHPUnit inside [WordPress Playground][playground]
(PHP-WASM + embedded SQLite). There is no host PHP, MySQL, or WordPress
installation to configure. Components only need a `tests/` directory with
PHPUnit test files.

[playground]: https://www.npmjs.com/package/@wp-playground/cli

## Running tests

```bash
# Test a single component (lint + PHPUnit)
homeboy test <component-id>

# Test an entire project (all components)
homeboy test <project-id>

# Verbose diagnostics
HOMEBOY_DEBUG=1 homeboy test <component-id>
```

Tests run through `scripts/test/test-runner.sh`, which dispatches to the
Playground runner (`test-runner-playground.sh` + `playground-runner.php`).
The runner mounts the component under `/wordpress/wp-content/plugins/<slug>`,
boots WordPress in-process, discovers test files, and runs PHPUnit.

## Requirements

A component needs:

- `tests/` directory with PHPUnit test files (default discovery:
  `*Test.php` suffix or `test-*` prefix, recursive).
- Plugin header (`Plugin Name:`) or theme `style.css` with `Theme Name:` —
  the runner detects which is which and loads accordingly.

A component **must not** carry its own `tests/bootstrap.php` or
`phpunit.xml` — the extension owns bootstrap. Local PHPUnit configs are
rejected with a clear error.

## Dependencies

If your plugin depends on other local plugins at runtime, declare them:

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "validation_dependencies": "data-machine, other-plugin"
      }
    }
  }
}
```

Dependencies are mounted alongside the plugin under test and their entry
files are loaded during the `load_deps` bootstrap stage.

## Drop-ins (`db.php`)

Plugins that ship a `db.php` drop-in are supported automatically. The
runner mounts `<plugin>/db.php` to `/wordpress/wp-content/db.php`;
Playground's built-in SQLite mu-plugin detects it and steps aside. See
[`PLAYGROUND_DROPIN.md`](PLAYGROUND_DROPIN.md) for the full coexistence
mechanism.

## Linting

Lint runs before PHPUnit. A lint failure aborts the test run with exit
code 1.

- **PHPCS** — `phpcs.xml.dist` applies WordPress Coding Standards (PSR-4
  adjustments applied). Text domain is auto-detected from the plugin
  header.
- **PHPStan** — `phpstan.neon.dist` runs static analysis with WordPress +
  WP-CLI + WooCommerce stubs. Critical-only by default.
- **ESLint** — runs only when JS/JSX/TS/TSX files exist in the component.
  WordPress ESLint config.

Components must not ship local `phpcs.xml`, `phpstan.neon`, or `.eslintrc`
— the extension owns the standards.

## Debug output

The Playground runner writes a structured log to
`<plugin>/.pg-test-result.txt` with line patterns like:

```
STAGE_BEGIN:<stage>       entering a bootstrap phase
STAGE_OK:<stage>          phase completed cleanly
STAGE_FAIL:<stage>:<msg>  Throwable caught during phase
STAGE_FATAL:<stage>:<msg> uncatchable fatal (shutdown handler)
NOTICE:<msg>              warning/notice surfaced from bootstrap
ALL TESTS PASSED          result.wasSuccessful() == true
SOME TESTS FAILED         result.wasSuccessful() == false
```

Stages run in order: `boot → install → load_fixtures → load_deps →
load_component → discover_tests → load_tests → run_tests`. The bash runner
parses these markers to classify failures; see `test-runner-playground.sh`
for the full classification table.

## Known gaps

- **WP version is pinned.** Currently `--wp=6.9`. Mismatched pins produce
  missing-class errors.
- **Partial phpunit.xml consumption.** The runner reads `<testsuite>` and
  `<exclude>` entries from `phpunit.xml.dist` only; other elements are
  ignored.
