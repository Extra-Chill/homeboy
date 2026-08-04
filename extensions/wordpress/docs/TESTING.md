# Testing

The WordPress extension runs PHPUnit through a selected real-WordPress runtime
backend. [WP Codebox][wp-codebox] is the default backend implementation and owns
the disposable WordPress runtime, mounts, command execution, logs, and test
artifacts. There is no host PHP, MySQL, or WordPress installation to configure.
Components only need a `tests/` directory with PHPUnit test files.

[wp-codebox]: https://github.com/Automattic/wp-codebox

## Running tests

```bash
# Test a single component (lint + PHPUnit)
homeboy test <component-id>

# Test an entire project (all components)
homeboy test <project-id>

# Verbose diagnostics
HOMEBOY_DEBUG=1 homeboy test <component-id>

# Rerun one real-WordPress host smoke through the CI-equivalent runtime path
homeboy test <component-id> -- --host-smoke-file tests/example-smoke.php
```

Tests run through `scripts/test/test-runner.sh`, which dispatches to the WP
Codebox implementation for WordPress PHPUnit by default. The runner mounts the
component under `/wordpress/wp-content/plugins/<slug>`, boots WordPress
in-process, discovers PHPUnit test files, and routes explicitly selected
diagnostic files by type.

Select the runtime backend with `HOMEBOY_WORDPRESS_TEST_RUNTIME_BACKEND` when a
runner needs to be explicit. Supported values: `wp-codebox` (default).

```bash
HOMEBOY_WORDPRESS_TEST_RUNTIME_BACKEND=wp-codebox homeboy test <component-id>
```

## WP Codebox command diagnostics

Recipe plan inputs can request command diagnostics capture by setting
`diagnosticsCapture`, `captureDiagnostics`, `commandDiagnostics`, or
`diagnostics` on the recipe generator options. A boolean value requests the
default evidence set, `queries` and `errors`; an array limits capture to the
listed evidence types.

Homeboy settings can pass the same plan through `wp_codebox_command_diagnostics`
or `command_diagnostics`.

Supported WordPress recipe commands receive the normalized plan on the step as
`diagnostics.capture`, allowing WP Codebox to attach query/error evidence to the
command result without Homeboy depending on product-specific behavior.

## Requirements

A component needs:

- `tests/` directory with PHPUnit test files (default discovery:
  `*Test.php` suffix or `test-*` prefix, recursive).
- Plugin header (`Plugin Name:`) or theme `style.css` with `Theme Name:` —
  the runner detects which is which and loads accordingly.

A component **must not** carry its own `tests/bootstrap.php` or
`phpunit.xml` — the extension owns bootstrap. Local PHPUnit configs are
rejected with a clear error.

## Real-WordPress host smokes

Standalone PHP smoke files can live under `tests/**/*-smoke.php`. They are
diagnostic/operator targets, not default release gates. Declare each smoke's
environment in a root `homeboy-test-manifest.json` using schema
`homeboy/test-manifest/v1`. Its `tests` object maps exact test paths to an
`environment` of `standalone-php` or `wordpress`. Undeclared PHP smokes default
to `wordpress`. `standalone-php` scripts run directly with PHP; `wordpress`
scripts use the selected runtime backend with component mounts, dependency
mounts, drop-in handling, WordPress version selection, and `wordpress.run-php`:

```json
{
  "schema": "homeboy/test-manifest/v1",
  "tests": {
    "tests/contract-smoke.php": { "environment": "standalone-php" }
  }
}
```

```bash
homeboy test <component-id> -- --host-smoke-file tests/example-smoke.php
```

Use `--file tests/contract-smoke.php` to run a declared standalone PHP smoke.
The real-WordPress focused path preserves the machine-readable `HOST_SMOKE_BEGIN`,
`HOST_SMOKE_PROGRESS`, `HOST_SMOKE_OK`, `HOST_SMOKE_FAIL`, and
`HOST_SMOKE_SUMMARY` markers, and fails fast with the selected script name.

## JavaScript unit tests

`*.test.js`, `*.test.jsx`, `*.test.mjs`, `*.test.cjs`, `*.test.ts`, and
`*.test.tsx` files route to the framework the component declares, not to a
backend chosen from the file extension. Selection order:

1. `HOMEBOY_WORDPRESS_JS_TEST_SCRIPT`
2. the `wordpress_js_test_script` (or `js_test_script`) setting
3. the first declared `package.json` script from
   `HOMEBOY_WORDPRESS_JS_TEST_SCRIPT_CANDIDATES`, which defaults to
   `test:unit test:unit:js test:js`

When one of those resolves, the runner delegates to the package manager
(`npm run <script> -- <selected files>`), which preserves the package's
config, transforms, setup files, globals, and test environment. Evidence
reports `Backend: package-script` with the `Contract:` line naming the source
that selected it, and `JS_TEST_BEGIN` / `JS_TEST_SUMMARY` markers.

```json
{
  "scripts": {
    "test:unit": "wp-scripts test-unit-js"
  }
}
```

Node's built-in runner (`Backend: node-test`) is used only when no script is
declared and every selected file imports `node:test`. A selected file that
declares neither is a hard error with actionable guidance rather than a
`describe is not defined` failure from the wrong runner.

## Agent Bundle Validator

Agent bundle repositories can run the shared bundle validator as a standalone
CI smoke without booting WordPress:

```bash
php path/to/homeboy-extensions/wordpress/scripts/agent/validate-bundle.php path/to/spec.json
```

The spec declares the bundle directory, expected manifest slugs, bundled
pipeline and flow files, required memory files, required AI tools, and optional
dot-path assertions against the manifest or example runner config. `bundle_dir`
and `example_runner_config` are resolved relative to the spec file's parent
directory so specs can live at repo root or under `tests/`.

For full CI agent runs on the WP Codebox WordPress execution substrate, use the
generic `runtime-agent-full-run.yml` workflow.

## WordPress test runtime status

WordPress PHPUnit files run through the selected runtime backend, with
`wp-codebox` as the current default implementation. Explicitly selected
standalone `tests/**/*-smoke.php` files use the same real-WordPress path for
component mounts, dependency mounts, drop-ins, file routing, WordPress version
selection, and artifact parsing when an operator chooses to run one.

## Dependencies

If your plugin depends on other local plugins at runtime, declare them:

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "validation_dependencies": "example-dependency, other-plugin"
      }
    }
  }
}
```

Dependencies are mounted alongside the plugin under test and their entry
files are loaded during the `load_deps` bootstrap stage.

Bench and trace scenarios preflight dependency plugin packages before WP
Codebox dispatch. The dependency path must be the runnable WordPress plugin
package root with a root `Plugin Name:` main file. Monorepo source checkouts,
such as a WooCommerce repository root where the plugin lives under
`plugins/woocommerce/woocommerce.php`, may need a packaged plugin build before
they can be used as WordPress runtime evidence. Preflight failures write
structured diagnostics to
`wordpress-dependency-plugin-preflight-diagnostics.json` in the run artifact
directory and distinguish missing paths, missing plugin main files, and plugin
load fatals caused by missing generated build artifacts.

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
  them up. **Errors block; warnings are reported (summary + findings) but
  non-blocking by default** (`ignore_warnings_on_exit 1`), so an
  error-clean repo releases without `--skip-checks=lint`. Set
  `HOMEBOY_LINT_FAIL_ON=warnings` to restore legacy block-on-warning
  behavior.
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
load_component → discover_tests → load_tests → run_tests`. The WP Codebox test
runner parses these markers to classify failures.

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
the WP Codebox bench command exits after workloads complete, so it does not keep
a browser-usable HTTP server alive for a later Playwright phase.

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

## WordPress bench crawl helper

`scripts/bench/lib/wordpress-bench-crawl.php` provides a generic helper for
benchmark workloads that need to crawl a bounded ordered list of WordPress URLs
or routes through the WordPress HTTP API. It is intended for WP Codebox-backed
WordPress workloads and does not require browser automation.

Minimal workload:

```php
<?php
require_once '/homeboy-extension/scripts/bench/lib/wordpress-bench-crawl.php';

return function (): array {
    return homeboy_wordpress_bench_crawl_payload(
        [
            '/',
            '/sample-page/',
            [ 'route' => '/wp-json/', 'method' => 'GET' ],
        ],
        [
            'batch_index' => 0,
            'max_requests' => 3,
            'timeout' => 10,
        ]
    );
};
```

The payload includes numeric crawl metrics for normal bench aggregation and
structured metadata under `metadata.wordpress_bench_crawl.rows`. Each row
contains `batch_index`, `request_index`, `url`, optional `route`, `method`,
`status`, `http_status`, `elapsed_ms`, optional `response_bytes`, and optional
`failure_message`.

## Playground HTTP readiness

`lib/playground-readiness.js` exports a Node helper for callers that boot a
WP Codebox/Playground HTTP server (or any WordPress origin) and need to wait
until it is actually serving traffic before driving Playwright, REST calls,
or load tests.

The helper is CommonJS, has no external dependencies, and lives alongside
`lib/request-profiler.js` because the readiness paths and Playground quirks
are WordPress-domain knowledge.

### `waitForWordPressReady(baseUrl, options)`

Polls a single readiness path until it returns a ready status, then resolves
with `{ status: 'ready', url, http_status, ready_reason, status_history,
redirect_history, elapsedMs }`. Resolves with `{ status: 'process_exited', ... }` if the
optional `playgroundProcess` exits before ready. Throws on timeout with a
populated `.diagnostics` property.

Options:

| Option | Default | Purpose |
|---|---|---|
| `path` | `/wp-json/` | Path to poll. Defaults to `/wp-json/` because Playground can return `302 Location: /` on `/`, which hangs Node's `fetch()` in a self-redirect loop. The helper uses `http.request()` directly and never follows redirects. |
| `readyStatus` | `200` | Single status or array of statuses considered ready. Strict by default; 3xx is not treated as ready. |
| `readyOnSelfRedirect` | `false` | Treat a same-origin, same-path redirect as ready. Use this only for browser-driving flows where the Playground login wrapper may return `302 Location: <same-path>` for every raw HTTP probe while Playwright browser navigation can still load the site. Keep this off for REST/load-test callers that require a real `200`. |
| `intervalMs` | `1000` | Delay between poll attempts. |
| `requestTimeoutMs` | `5000` | Per-request abort. |
| `timeoutMs` | `120000` | Total budget before throwing. |
| `playgroundProcess` | `null` | Optional `ChildProcess`-like with `.exitCode` / `.signalCode`. If it exits before ready, the helper resolves with `process_exited` instead of throwing. |
| `playgroundOutput` | `null` | Optional `() => string` returning captured stdout/stderr. The last 4000 chars are attached to the timeout artifact for debugging. |
| `onEvent` | `null` | Optional `(source, event, data) => void`. Emits `http.first_response`, `http.status`, `http.redirect`, `http.ready`, `http.timeout`, `process.exited`. |

### `probeWordPressDiagnostics(baseUrl, options)`

One-shot diagnostic probe over a fixed set of WordPress paths. Returns
`{ origin, paths: [{ path, url, status, contentType, redirectLocation,
bodyPreview, elapsedMs, error?, tcp? }, ...] }`. Used internally to
populate the timeout artifact, but exposed for callers that want a
standalone snapshot. Defaults to probing
`['/', '/wp-login.php', '/wp-json/', '/wp-admin/']`. Manual redirect
handling — never follows `Location`.

### Why the default path is `/wp-json/`, not `/`

Playground may respond to `/` with `302 Location: /`, a self-redirect. Node's built-in `fetch()` treats `Location` as
follow-by-default, so a naive `fetch(baseUrl)` against a Playground
server hangs until the abort signal fires. Polling `/wp-json/` instead
returns a clean `200 application/json` once WordPress has finished
booting, with no redirect involved. The helper also uses
`http.request()` rather than `fetch()` so even the `/` poll path stays
single-attempt.

When the Playground login wrapper is used, it may also return a same-path
redirect for `/wp-json/` and other raw HTTP probes.
Browser-based flows can pass `readyOnSelfRedirect: true` to treat that shape
as server readiness before driving Playwright. The option is explicit so REST
and load-test flows do not mistake a login wrapper redirect for a usable API
response.

### Timeout artifact shape

```json
{
  "url": "http://127.0.0.1:9400/wp-json/",
  "timeoutMs": 120000,
  "attempts": 17,
  "lastError": null,
  "recentAttempts": [
    { "at_ms": 1010, "status": 503 },
    { "at_ms": 2020, "status": 503 }
  ],
  "redirect_history": [],
  "status_history": [{ "status": 503, "count": 17 }],
  "routes": {
    "origin": "http://127.0.0.1:9400",
    "paths": [
      { "path": "/", "url": "http://127.0.0.1:9400/", "status": 302, "redirectLocation": "/", "bodyPreview": "...", "elapsedMs": 12 }
    ]
  },
  "tcp": { "open": true },
  "playground": { "pid": 12345, "exitCode": null, "signalCode": null },
  "playgroundOutputTail": "...last 4000 chars of captured stdout/stderr..."
}
```

### Usage

```js
const { waitForWordPressReady } = require('homeboy-extension-wordpress/playground-readiness');

const child = spawnPlaygroundCli({ port: 9400 });
const captured = [];
child.stdout.on('data', (chunk) => captured.push(chunk));
child.stderr.on('data', (chunk) => captured.push(chunk));

const result = await waitForWordPressReady('http://127.0.0.1:9400', {
  playgroundProcess: child,
  playgroundOutput: () => Buffer.concat(captured).toString('utf8'),
  timeoutMs: 60000,
});

if (result.status === 'process_exited') {
  throw new Error(`Playground exited before ready: code=${result.playground.exitCode}`);
}
// result.status === 'ready' — drive Playwright/REST against result.url
```

## Known gaps

- **WP version defaults to 6.9.** Override `wordpress_runtime_version` to pass
  a different WordPress version to WP Codebox. Mismatched versions produce
  missing-class errors.
- **Multisite runtime.** Plugin PHPUnit recipes provision a multisite runtime
  when any of the following request it, in precedence order:
  1. `HOMEBOY_WORDPRESS_MULTISITE=1` (env)
  2. `wp_codebox_multisite: true` in settings (set `false` to force single-site
     even for a network plugin)
  3. the plugin's own `Network: true` header (auto-detected, zero config)

  The selected runtime is reported to WP Codebox via the `multisite` recipe
  argument (`0` single-site, `1` multisite), so network-only plugins that
  `wp_die()` outside multisite boot correctly.
- **Partial phpunit.xml consumption.** The runner reads `<testsuite>` and
  `<exclude>` entries from `phpunit.xml.dist` only; other elements are
  ignored.
