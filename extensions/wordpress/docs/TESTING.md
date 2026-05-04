# Testing

The WordPress extension runs PHPUnit inside [WordPress Playground][playground]
(PHP-WASM + embedded SQLite) by default. There is no host PHP, MySQL, or
WordPress installation to configure. Components only need a `tests/` directory
with PHPUnit test files.

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
configured backend. The default Playground runner (`test-runner-playground.sh`
and `playground-runner.php`) mounts the component under
`/wordpress/wp-content/plugins/<slug>`, boots WordPress in-process, discovers
test files, and runs PHPUnit.

## Requirements

A component needs:

- `tests/` directory with PHPUnit test files (default discovery:
  `*Test.php` suffix or `test-*` prefix, recursive).
- Plugin header (`Plugin Name:`) or theme `style.css` with `Theme Name:` —
  the runner detects which is which and loads accordingly.

A component **must not** carry its own `tests/bootstrap.php` or
`phpunit.xml` — the extension owns bootstrap. Local PHPUnit configs are
rejected with a clear error.

## Host smoke backend

Pure PHP smoke suites can opt out of Playground and run directly under host
PHP:

```bash
homeboy component set <component-id> test_backend host-smoke
```

The host-smoke backend discovers `tests/**/*-smoke.php`, runs each script in a
separate `php` process, emits `HOST_SMOKE_*` markers, and fails fast with the
failing script name. It does not bootstrap WordPress, connect to MySQL, or start
Playground.

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
  header. When PHPCS reports auto-fixable findings, the runner surfaces a
  prominent CTA showing the exact `homeboy refactor` command to clean
  them up.
- **PHPStan** — `phpstan.neon.dist` runs static analysis at **level 7**
  with WordPress + WP-CLI + WooCommerce stubs. Level 7 unlocks argument
  type flow analysis (catches `false` / `null` leaking into strict-typed
  callees). `missingType.*` identifiers are suppressed by default to keep
  the signal-to-noise ratio high. Tests are analyzed alongside source.
- **ESLint** — runs only when JS/JSX/TS/TSX files exist in the component.
  WordPress ESLint config.

Components must not ship local `phpcs.xml`, `phpstan.neon`, or `.eslintrc`
— the extension owns the standards. The one exception is PHPStan
baselines (see below).

### PHPStan baselines

Components with a lot of pre-existing findings can capture a baseline to
keep the harness green while fixing findings incrementally. Generate one
from inside the component directory:

```bash
path/to/extension/vendor/bin/phpstan analyse \
    --configuration=path/to/extension/phpstan.neon.dist \
    --level=7 --memory-limit=2G \
    --generate-baseline=phpstan-baseline.neon \
    .
```

Commit the resulting `phpstan-baseline.neon` at the component root. The
runner detects it and pulls it into the analysis via `includes:`. New
code must not add new findings — existing findings are grandfathered in,
and new ones fail the run. Delete the baseline to ratchet toward full
cleanup.

### Sanctioned lint suppressions

Prefer fixing real findings over suppressing them. When a finding is caused by
a deliberately loose WordPress/runtime boundary, use a narrow, grep-friendly
suppression on the line immediately before the finding. Include the tool rule
identifier and a short runtime-contract justification.

For defensive guards on untyped public APIs, keep the guard when the method is
intentionally callable by external consumers that may pass a falsey value even
though this component's own call graph does not. Use the PHPStan identifier so
the suppression remains narrow:

```php
// @phpstan-ignore-next-line booleanNot.alwaysFalse -- Defensive public API guard for untyped external callers.
if ( ! $element ) {
    return null;
}
```

For display-only redirect-result reads, suppress only the nonce recommendation
on the sanitized read. This is appropriate for admin notices or other display
branches that do not mutate state and whose corresponding action already
verified its nonce before redirecting:

```php
// phpcs:ignore WordPress.Security.NonceVerification.Recommended -- Display-only redirect-result read for admin notice.
$result = isset( $_GET['example_result'] ) ? sanitize_text_field( wp_unslash( $_GET['example_result'] ) ) : '';
```

For minimum-PHP runtime guards, keep the guard in plugin bootstrap code. The
analyzer runs against a configured PHP target, but WordPress can still load the
plugin on older hosts before the plugin has a chance to bail safely:

```php
// @phpstan-ignore-next-line if.alwaysFalse -- Runtime guard is required for installs below the analysis PHP target.
if ( version_compare( PHP_VERSION, EXAMPLE_MIN_PHP, '<' ) ) {
    return;
}
```

Do not use broad PHPCS disables, broad PHPStan baselines, or identifier-less
`@phpstan-ignore-next-line` comments for these cases. The goal is to preserve
the useful lint signal everywhere except the single intentional boundary.

### Level override

`HOMEBOY_PHPSTAN_LEVEL=8 homeboy test <component>` bumps one-off without
changing the default. `HOMEBOY_SKIP_PHPSTAN=1` runs a critical-only
check that still blocks `function.notFound` / `class.notFound` (guaranteed
runtime fatals) regardless of the skip.

### Multi-line comment style (opt-in)

Custom sniff `HomeboyWordPress.Commenting.MultiLineInlineComment` enforces
[WordPress Inline Documentation Standards section 5.2][wp-docs-5.2]:

- **`ConsecutiveSingleLine`** — flags 2+ adjacent `//` lines that should be
  a `/* ... */` block comment. Auto-fixable.
- **`DoubleAsteriskNonDocBlock`** — flags `/**` blocks used for prose, not
  for declarations. The double-asterisk form is reserved for DocBlocks per
  WP handbook; bare prose should start with a single asterisk. Auto-fixable.

Both detections skip `// phpcs:`, `// translators:`, `//phpstan-`, `//psalm-`
annotations, end-of-line trailing comments, and runs that look entirely like
commented-out code (deferred to `Squiz.PHP.CommentedOutCode`).

The sniff is **registered but off by default** to avoid a flag-day on
existing projects. Three ways to opt in:

1. Promote a single error code in your project ruleset:

   ```xml
   <rule ref="HomeboyWordPress.Commenting.MultiLineInlineComment.ConsecutiveSingleLine">
       <severity>5</severity>
   </rule>
   ```

2. Promote the whole sniff:

   ```xml
   <rule ref="HomeboyWordPress.Commenting.MultiLineInlineComment">
       <severity>5</severity>
   </rule>
   ```

3. Run on demand without touching the host ruleset:

   ```bash
   vendor/bin/phpcs --sniffs=HomeboyWordPress.Commenting.MultiLineInlineComment src/
   vendor/bin/phpcbf --sniffs=HomeboyWordPress.Commenting.MultiLineInlineComment src/
   ```

[wp-docs-5.2]: https://developer.wordpress.org/coding-standards/inline-documentation-standards/php/#5-2-multi-line-comments

### Strict `empty()` / `isset()` enforcement (opt-in)

By default, PHPStan at level 7 already catches the *provably wrong* uses
of `empty()` / `isset()` via built-in rules (`empty.variable`,
`empty.offset`, `isset.variable`, `isset.offset`, `isset.property`).
That covers every case where PHPStan can prove the check is redundant
or a bug.

Teams that want to **ban all uses of `empty()`** — matching the
[WordPress/performance plugin's stance][wp-perf-1803] — can opt in to
[`phpstan/phpstan-strict-rules`][strict-rules] locally. Install it per
component:

```bash
composer require --dev phpstan/phpstan-strict-rules
```

Then add a component-local `phpstan.neon` pointing at the strict rules:

```neon
includes:
    - vendor/phpstan/phpstan-strict-rules/rules.neon
```

This enables `empty.notAllowed` plus ~20 other strict rules
(`disallowedLooseComparison`, `booleansInConditions`, `uselessCast`,
`noVariableVariables`, etc.). Generating a baseline first is strongly
recommended — real-world components typically light up with hundreds to
thousands of findings across the whole strict set.

This is not shipped as a default because the strict-rules package
activates a broad set of toggles that can't be disabled from the
consumer config (`checkDynamicProperties`, `reportMaybesInMethodSignatures`,
etc.), producing an order-of-magnitude finding explosion on most
codebases. Opt-in keeps the default honest while leaving the door open
for teams that want full enforcement.

[wp-perf-1803]: https://github.com/WordPress/performance/pull/1803
[strict-rules]: https://github.com/phpstan/phpstan-strict-rules

### Test-smell preflight

Before Playground boots PHPUnit, the runner scans `tests/**/*.php` for the
high-confidence `wp.test.mock_over_fixture` smell: a test method that creates
`$query = new WP_Query()` and then manually assigns result-state fields such
as `->posts`, `->post_count`, or `->found_posts`.

That pattern mocks WordPress query internals even though the WordPress test
fixture layer can build the state directly. Prefer real fixtures:

```php
$post_id = self::factory()->post->create( array( 'post_date' => $date ) );
query_posts( array( 'posts__in' => array( $post_id ), 'fields' => 'ids' ) );
```

Use `WP_Query` with real query args when a local query object is required.
If a test intentionally exercises impossible query state, suppress the single
case with `// homeboy-ignore wp.test.mock_over_fixture` near the setup.

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

## Browser bench target handoff

Browser benchmarks are a two-extension handoff, not a second WordPress bench
runner. The WordPress extension prepares/describes the WordPress target; the
Node extension browser helper owns Playwright, browser metrics, screenshots, and
browser artifacts.

When a component enables `bench_browser_target`, the WordPress bench runner writes
this file:

```text
${HOMEBOY_BENCH_SHARED_STATE}/browser-target.json
```

Minimum component setting:

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "bench_browser_target": { "enabled": true }
      }
    }
  }
}
```

The v1 file shape is:

```json
{
  "schemaVersion": 1,
  "kind": "wordpress",
  "lifecycle": {
    "server": "external",
    "keepAlive": "caller"
  },
  "baseUrl": "http://127.0.0.1:8888/",
  "adminUrl": "http://127.0.0.1:8888/wp-admin/",
  "login": {
    "method": "credentials",
    "username": "admin",
    "password": "..."
  },
  "metadata": {
    "wpVersion": "6.9",
    "componentId": "my-plugin",
    "pluginSlug": "my-plugin",
    "benchSiteMode": "installed"
  },
  "artifactPolicy": {
    "publishRaw": false,
    "secretFields": ["login.password", "login.url"]
  }
}
```

Use `baseUrl` / `adminUrl` when an installed or persisted site is already served
by the caller. If `baseUrl` is omitted, the target file is **metadata only**:
the current Playground PHP runner runs `wp-playground-cli php` and exits after
workloads complete, so it does not keep a browser-usable HTTP server alive for a
later Playwright phase.

Credentials may be supplied directly or via an environment variable indirection:

```json
{
  "bench_browser_target": {
    "enabled": true,
    "baseUrl": "http://127.0.0.1:8888/",
    "login": {
      "method": "credentials",
      "username": "admin",
      "password_env": "WP_BROWSER_PASSWORD"
    }
  }
}
```

The raw `browser-target.json` file is a handoff artifact, not a report artifact.
It can contain credentials or auto-login URLs; runners must not print it to logs
or publish it with benchmark results without redacting the fields listed in
`artifactPolicy.secretFields`.

## Known gaps

- **WP version is pinned.** Currently `--wp=6.9`. Mismatched pins produce
  missing-class errors.
- **Partial phpunit.xml consumption.** The runner reads `<testsuite>` and
  `<exclude>` entries from `phpunit.xml.dist` only; other elements are
  ignored.
