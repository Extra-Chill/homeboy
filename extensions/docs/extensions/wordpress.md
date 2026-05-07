# WordPress Extension

## Component Shapes

The WordPress extension supports two component shapes:

- `standalone` / default — WordPress plugins and themes. Tests run through WordPress Playground with the component mounted under `wp-content/plugins/<slug>` or the existing theme/plugin path assumptions.
- `core-dev` — a `wordpress-develop` checkout. Tests, lint, and build dispatch to WordPress core's native tooling instead of mounting the checkout into Playground.

Homeboy core may pass `HOMEBOY_COMPONENT_SHAPE=core-dev` for registered components. For direct script execution and smoke tests, the extension also detects `wordpress-develop` by the marker set `wp-config-sample.php`, `src/wp-includes/version.php`, and `tests/phpunit/`.

The core-dev runner expects WordPress core's own dependencies and config. It installs missing npm/composer dependencies, builds `src/` into `build/`, and runs PHPUnit through core's `vendor/bin/phpunit`. If `wp-tests-config.php` is missing, set `HOMEBOY_WP_TESTS_DB_NAME`, `HOMEBOY_WP_TESTS_DB_USER`, `HOMEBOY_WP_TESTS_DB_PASSWORD`, and optionally `HOMEBOY_WP_TESTS_DB_HOST` so the runner can write it from the sample config.

## Test failure sidecar

When Homeboy sets `HOMEBOY_TEST_FAILURES_FILE`, the WordPress PHPUnit runners write a JSON sidecar with parsed failure details. Existing Homeboy analysis fields are preserved, and each failure also includes normalized sidecar fields for cross-runner consumers:

```json
{
  "total": 4,
  "passed": 3,
  "failures": [
    {
      "test_name": "Vendor\\Package\\ExampleTest::test_example",
      "test_file": "tests/ExampleTest.php",
      "error_type": "AssertionFailedError",
      "message": "Failed asserting that false is true.",
      "source_file": "src/Example.php",
      "source_line": 42,
      "test_id": "Vendor\\Package\\ExampleTest::test_example",
      "suite": "phpunit",
      "file": "src/Example.php",
      "line": 42,
      "failure_type": "AssertionFailedError",
      "fingerprint": "...",
      "stdout_excerpt": "Vendor\\Package\\ExampleTest::test_example\nFailed asserting that false is true.",
      "stderr_excerpt": ""
    }
  ]
}
```

`file` and `line` point to the parsed source location when available, falling back to the test file and line `0`. `fingerprint` is a stable SHA-256 grouping key based on the test id, normalized location, failure type, and first message line.

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

## Configurable Playground Bench Workloads

WordPress bench runs can declare Playground workloads in extension settings when
the workload should be configured by the repo instead of living under
`tests/bench/*.php`. Configured workloads run after the existing Playground
bootstrap, `playground_blueprint`, dependency mounts, and component load.

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "playground_workloads": [
          {
            "id": "generated-site-preview",
            "label": "Generated site preview",
            "run": [
              {
                "type": "php",
                "file": "workloads/generated-site-preview.php"
              }
            ],
            "artifacts": {
              "import_report": {
                "path": "wp-content/themes/example/import-report.json",
                "kind": "json",
                "label": "Import report"
              }
            },
            "metadata": {
              "preview_url": "https://example.test/preview"
            }
          }
        ]
      }
    }
  }
}
```

Supported step types:

- `php` with `file` or `code`: runs inside the Playground PHP process. Files are
  resolved relative to the mounted component path unless absolute.
- `ability` with `ability` (and optional `input`, `user`): resolves the named
  ability via `wp_get_ability()` (WordPress core 6.9+) and executes it inside
  the Playground PHP process. The runner fires `wp_abilities_api_categories_init`
  and `wp_abilities_api_init` before the first ability call so plugin-declared
  categories and abilities land in the registry. Use this for plugins that
  expose their entry points as abilities so workloads don't need a WP-CLI
  command surface.
- `wp-cli` with `command`: runs through `WP_CLI::runcommand()` when WP-CLI is
  available in the Playground PHP process. The command may include or omit the
  leading `wp` token.

Workloads and steps may return `{ "metrics", "artifacts", "metadata" }`.
Numeric metrics are aggregated across measured iterations with the same
mean/p50/p95/p99/min/max suffixes used by PHP bench files. Artifacts and metadata
are carried into the Homeboy BenchResults scenario envelope.

Example: drive a plugin's pipeline through an Abilities API entry point.

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "playground_blueprint": {
          "steps": [
            { "step": "installPlugin", "pluginData": { "resource": "wordpress.org/plugins", "slug": "data-machine" } }
          ]
        },
        "playground_workloads": [
          {
            "id": "smoke-pipeline",
            "run": [
              {
                "type": "ability",
                "ability": "datamachine/run-pipeline",
                "input": { "pipeline_id": 42 }
              }
            ]
          }
        ]
      }
    }
  }
}
```

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

## Request Profiler Helper

The WordPress extension exports a Node helper for bench and trace workloads that
need server-side WordPress request timing. It installs a temporary MU-plugin into
a target WordPress site, preserves JSONL profile entries on disk, parses those
entries after the workload runs, and removes the profiler when requested.

```js
const {
  installWordPressRequestProfiler,
  collectWordPressRequestProfiles,
  uninstallWordPressRequestProfiler,
} = require('homeboy-extension-wordpress/request-profiler');

const sitePath = '/path/to/wordpress';

installWordPressRequestProfiler(sitePath);

// Run one or more browser, curl, WP-CLI, bench, or trace requests here.

const entries = collectWordPressRequestProfiles(sitePath);
uninstallWordPressRequestProfiler(sitePath);

console.log(entries.filter((entry) => entry.event === 'http.request.start'));
```

By default the helper writes `wp-content/homeboy-profile.jsonl` and installs
`wp-content/mu-plugins/homeboy-request-profiler.php`. The JSONL file is left in
place during cleanup so benchmark and trace runners can preserve it as an
artifact. Pass `{ removeArtifact: true }` to `uninstallWordPressRequestProfiler`
when the raw profile should also be deleted.

Captured entries include:

- request start timing and request metadata
- WordPress lifecycle hook marks such as `muplugins_loaded`, `plugins_loaded`,
  `init`, `admin_init`, `current_screen`, `admin_enqueue_scripts`, and `shutdown`
- priority-band start/end marks around `admin_init`, `current_screen`, and
  `admin_enqueue_scripts`
- outbound HTTP request starts from `pre_http_request`, including hashed IDs,
  URLs, and methods

The default hooks can be overridden when a workload needs a smaller or more
specific profile:

```js
installWordPressRequestProfiler(sitePath, {
  artifactRelativePath: 'wp-content/uploads/homeboy/admin-profile.jsonl',
  hooks: ['init', 'admin_init', 'shutdown'],
  priorityBandHooks: ['admin_init'],
});
```
