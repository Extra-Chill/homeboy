# WordPress Extension

## Component Shapes

The WordPress extension supports two component shapes:

- `standalone` / default — WordPress plugins and themes. Tests run through WordPress Playground with the component mounted under `wp-content/plugins/<slug>` or the existing theme/plugin path assumptions.
- `core-dev` — a `wordpress-develop` checkout. Tests, lint, and build dispatch to WordPress core's native tooling instead of mounting the checkout into Playground.

Homeboy core may pass `HOMEBOY_COMPONENT_SHAPE=core-dev` for registered components. For direct script execution and smoke tests, the extension also detects `wordpress-develop` by the marker set `wp-config-sample.php`, `src/wp-includes/version.php`, and `tests/phpunit/`.

The core-dev runner expects WordPress core's own dependencies and config. It installs missing npm/composer dependencies, builds `src/` into `build/`, and runs PHPUnit through core's `vendor/bin/phpunit`. If `wp-tests-config.php` is missing, set `HOMEBOY_WP_TESTS_DB_NAME`, `HOMEBOY_WP_TESTS_DB_USER`, `HOMEBOY_WP_TESTS_DB_PASSWORD`, and optionally `HOMEBOY_WP_TESTS_DB_HOST` so the runner can write it from the sample config.

## Codex WP Codebox Stack

Codex WP Codebox tasks require an explicit provider/runtime stack. Homeboy does not infer these paths from local worktree names, and it does not fetch provider PR branches itself before dispatch.

The WP Codebox agent runtime is exposed at `agent-runtimes/wp-codebox`. It carries the
provider contract, task request mapping, runtime CLI, and normalized outcome
conversion so the WordPress extension can depend on generic runtime capabilities
instead of embedding the provider contract.

Provider discovery lives in the runtime manifest's
`provider_metadata` block. Operators and generic tooling should read that block to
separate executor backend (`codebox`), runtime id (`wp-codebox`), provider id
(`openai`, `codex`, `claude-code`, or another WordPress AI provider), and model
selection (`executor.config.model` / `executor.model`). Homeboy core should treat
provider ids and model names as opaque runtime metadata.

Configure the Codex pair with task config, global settings, or environment variables:

- `provider_plugin_paths`: a Codex-capable provider plugin checkout, such as the Codex provider branch of `ai-provider-for-openai`. Legacy compatibility aliases: `wp_codebox_provider_plugin_paths`, `HOMEBOY_WP_CODEBOX_PROVIDER_PLUGIN_PATH`.
- `runtime_overlays`, or `php_ai_client_path`: a prepared `php-ai-client` checkout mounted to `/wordpress/wp-includes/php-ai-client`. Legacy compatibility aliases: `wp_codebox_runtime_overlays`, `wp_codebox_php_ai_client_path`, `HOMEBOY_WP_CODEBOX_PHP_AI_CLIENT_PATH`. Explicit runtime overlay entries must use the canonical `kind` field, for example `{ "kind": "bundled-library", "library": "php-ai-client", "source": "/abs/path/to/php-ai-client" }`; legacy `type` entries are rejected before WP Codebox dispatch.

The `php-ai-client` checkout must include bearer-token auth support (`RequestAuthenticationMethod::bearerToken`) and Composer vendor dependencies (`vendor/autoload.php`). If the stack is incomplete, the executor emits diagnostics for the missing Codex provider plugin, missing bearer-token auth, or missing Composer vendor preparation.

## WP Codebox Artifact Lookup

WordPress helpers that consume WP Codebox browser or recipe artifacts should use
the exported `homeboy-extension-wordpress/wp-codebox-artifacts` module instead
of parsing bundle directory layouts directly. The helper resolves artifact
references from returned runtime metadata (`artifacts`, `files`,
`artifactFiles`, nested `artifact.files`, summary file maps, or fallback paths)
to absolute files under the artifact bundle directory. It accepts canonical
directory aliases including `artifacts.directory`, `artifacts.path`,
`artifactsDirectory`, `artifacts_directory`, `artifactDirectory`, and
`artifact_directory`.

This keeps extension helpers product-neutral: workloads and probes can name the
artifact they need, while runtime-specific bundle layouts remain behind one
lookup boundary.

The generic `homeboy-extension-wordpress` root export does not flatten
WP Codebox helpers into the public WordPress API. Compatibility consumers that
need Codebox-owned helpers from the root export should access them through the
explicit `wpCodebox` namespace; new imports should prefer the dedicated
`homeboy-extension-wordpress/wp-codebox-*` subpath exports.

## Static Visual Parity Runtime Boundary

`wordpress/lib/static-visual-parity.js` keeps the static visual parity
orchestration API stable while routing runtime-specific work through a provider
object. The default provider is WP Codebox and is exposed explicitly through
`createWpCodeboxStaticVisualParityRuntimeProvider()`,
`buildWpCodeboxStaticVisualParityRecipe()`,
`runWpCodeboxStaticVisualParity()`, and
`normalizeWpCodeboxStaticVisualParityArtifacts()`.

Existing callers can continue using `buildStaticVisualParityRecipe()`,
`runStaticVisualParity()`, and `normalizeStaticVisualParityArtifacts()`; those
aliases select the WP Codebox provider unless a caller passes `runtimeProvider`.
The provider owns the `wp-codebox/workspace-recipe/v1` recipe shape, recipe file
name, runtime output file name, dispatch function, and artifact normalization.

## Audit Fanout Runtime Boundary

Audit fanout extraction is split from runtime execution. Generic fanout planning
and reconcile primitives live in `runtime-agent-ci/lib`; they group items,
template opaque task requests, and match provider records back to groups. The exported
`audit-fanout-runtime-provider` interface defines dispatch/apply operations
without naming runtime package names, provider credentials, sandbox recipes, or
provider task schemas.

WP Codebox is the current runtime provider implementation for audit fanout. The
quarantined `audit-wp-codebox-fanout` module and CLI map grouped audit findings
to `wp-codebox/task-input/v1`, execute those requests through Codebox-owned task
runner contracts, and normalize Codebox artifacts/outcomes back into fanout
records. Keep new executor-neutral extraction behavior in
`runtime-agent-ci/lib/generic-fanout-reconcile-workflow.js`; keep Codebox request/session/artifact
details inside the Codebox audit fanout lane.

## Static Site Fanout Adapter

The static-site fanout adapter emits generic `homeboy/agent-task-request/v1`
requests by default. It does not select WP Codebox or any other executor backend
unless the caller passes an explicit `backend`/`runtime_backend` value or agent
task backend override.

WP Codebox task-input compatibility is an opt-in compatibility path for callers
that still consume `wp-codebox/task-input/v1`. Select it with
`compatibility_provider: "wp-codebox"` or the legacy
`request_kind: "wp-codebox"` flag. New callers should prefer the generic
agent-task request schema and make runtime selection outside the adapter.

## WordPress Hook Surface Discovery

`wordpress-hook-surface-discovery` statically extracts literal WordPress
`add_action()`, `add_filter()`, `wp_schedule_event()`, and
`wp_schedule_single_event()` calls for generic fuzz planning. The helper reports
surface metadata and conservative invocation guidance without product-specific
semantics. Zero-argument actions are emitted as automatic `do_action` candidates;
filters, cron events, dynamic hook names, and callbacks that accept arguments are
preserved as skipped planning records with explicit `skip_reason` metadata so
callers can add fixtures before invoking them.

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
        "validation_dependencies": "example-dependency"
      }
    }
  }
}
```

Supported value shapes:

- single component ID: `example-dependency`
- comma-separated list: `example-dependency, other-plugin`
- newline-separated list
- JSON-array string: `["example-dependency", "other-plugin"]`

Each dependency entry may be either:

- a registered Homeboy component ID
- an absolute path to another local plugin checkout

## Configurable WP Codebox Bench Workloads

WordPress bench runs can declare runtime workloads in extension settings when
the workload should be configured by the repo instead of living under
`tests/bench/*.php`. Configured workloads run after the runtime bootstrap,
blueprint, dependency mounts, and component load through a generated WP Codebox
recipe. The current bench runner still consumes the legacy `wp_codebox_*`
settings for recipe-specific fields.

### Portable workload profile helper

Consumers that need the same WordPress workload shape across GitHub Actions,
bench scripts, and agent runners can describe the workload once with schema
`homeboy/wordpress-workload-profile/v1` and normalize it through
`wordpress/lib/wordpress-workload-profile.js`. The helper maps a generic profile
onto the reusable workflow inputs Homeboy Extensions already owns:

- `dependencies` become `validation_dependencies` entries.
- `wp_config_defines` becomes `extra_wp_config_defines` JSON.
- `mounts` becomes `runtime_mounts` JSON.
- `run_before` and `run_after` become workload lifecycle hook arrays.
- `workloads` becomes legacy `wp_codebox_workloads` JSON for the current WP
  Codebox bench recipe generator.
- `visual_comparisons` append generic `visual-compare` verifier steps to
  `workload_run_after`.

Example profile:

```json
{
  "schema": "homeboy/wordpress-workload-profile/v1",
  "id": "static-import-visual-check",
  "label": "Static import visual check",
  "dependencies": ["example/static-importer@main"],
  "mounts": [
    "/tmp/source:/wordpress/wp-content/uploads/source:readonly"
  ],
  "run_before": [
    { "type": "wp-cli", "command": "plugin install safe-svg --activate" }
  ],
  "workloads": [
    {
      "id": "import-and-snapshot",
      "run": [
        {
          "type": "ability",
          "ability": "example/import-static-site",
          "input": { "source": "/wordpress/wp-content/uploads/source" }
        }
      ],
      "artifacts": {
        "import_report": {
          "path": "wp-content/uploads/import-report.json",
          "kind": "json"
        }
      }
    }
  ],
  "visual_comparisons": [
    {
      "id": "home-page",
      "source_url": "https://source.example/",
      "candidate_url": "https://candidate.example/",
      "threshold": 0.01
    }
  ]
}
```

The profile is intentionally product-agnostic: import, fixture setup, crawling,
and verification are ordinary WordPress recipe steps, while visual comparison is
carried as a verifier step that a runtime can implement with its own browser
capture and artifact policy.

### Portable fuzz manifest helper

Fuzz callers that need one manifest across local scripts, CI, and agent runners
can use schema `homeboy/wordpress-fuzz-manifest/v1` and normalize it through
`wordpress/lib/wordpress-fuzz-manifest.js`. The manifest reuses the portable
workload profile fields for setup/execution and embeds the generic WordPress
fuzz discovery/plan contracts described below.

The helper returns a stable manifest object with:

- `workload_profile` normalized by `homeboy/wordpress-workload-profile/v1`.
- `discovery` normalized by `wordpress-surface-discovery/v1`, when provided.
- `plan` normalized by `wordpress-fuzz-plan/v1`, when provided.
- `artifacts`, `budget`, and `metadata` copied as product-agnostic execution
  hints.

`workflowInputsFromWordPressFuzzManifest()` projects the same runtime inputs as
the workload profile helper and adds JSON inputs for `wordpress_fuzz_manifest`,
`wordpress_fuzz_discovery`, `wordpress_fuzz_plan`, and
`wordpress_fuzz_artifacts`.

Example manifest:

```json
{
  "schema": "homeboy/wordpress-fuzz-manifest/v1",
  "id": "generic-rest-fuzz",
  "label": "Generic REST fuzz",
  "dependencies": ["example/plugin-under-test@main"],
  "run_before": [
    { "type": "wp-cli", "command": "rewrite flush" }
  ],
  "workloads": [
    {
      "id": "execute-fuzz-plan",
      "run": [
        {
          "type": "ability",
          "ability": "wordpress/fuzz-run",
          "input": { "plan_id": "generic-rest-plan" }
        }
      ]
    }
  ],
  "discovery": {
    "id": "generic-surfaces",
    "surfaces": [
      {
        "type": "rest-route",
        "id": "wp-v2-posts",
        "method": "GET",
        "route": "/wp/v2/posts"
      }
    ]
  },
  "plan": {
    "id": "generic-rest-plan",
    "discovery_id": "generic-surfaces",
    "targets": [
      {
        "id": "posts-list",
        "surface_id": "wp-v2-posts",
        "cases": [
          { "id": "per-page-boundary", "query": { "per_page": 100 } }
        ]
      }
    ]
  },
  "artifacts": [
    { "path": "artifacts/fuzz/result.json", "kind": "json" }
  ],
  "budget": { "max_cases": 25 }
}
```

The manifest is intentionally product-agnostic. Product-specific target
selection, fixture content, and assertions belong in caller-owned manifests or
runtime inputs, not in the shared contract.

### Surface discovery and fuzz schemas

The WordPress extension exposes product-agnostic data shapes for discovering
WordPress surfaces and exchanging fuzz plans/results. These are schema contracts
only; they do not select a runtime, generate cases, or execute fuzzing.

- `wordpress-surface-discovery/v1` lists WordPress surfaces such as REST routes,
  admin pages, post types, taxonomies, roles, capabilities, hooks, blocks,
  options, cron events, database tables, frontend URLs, and WP-CLI commands.
- `wordpress-fuzz-plan/v1` groups fuzz cases by discovered surface target.
- `wordpress-fuzz-result/v1` reports normalized case outcomes and summaries.

```json
{
  "schema": "wordpress-surface-discovery/v1",
  "id": "site-surfaces",
  "surfaces": [
    {
      "id": "wp-v2-posts",
      "type": "rest-route",
      "method": "GET",
      "route": "/wp/v2/posts"
    }
  ]
}
```

```json
{
  "schema": "wordpress-fuzz-plan/v1",
  "id": "rest-route-fuzz",
  "discovery_id": "site-surfaces",
  "targets": [
    {
      "id": "posts-list-query",
      "surface_id": "wp-v2-posts",
      "method": "GET",
      "route": "/wp/v2/posts",
      "cases": [
        { "id": "per-page-max", "query": { "per_page": 100 } }
      ]
    }
  ],
  "budget": { "max_cases": 25 }
}
```

```json
{
  "schema": "wordpress-fuzz-result/v1",
  "id": "rest-route-fuzz-result",
  "plan_id": "rest-route-fuzz",
  "status": "passed",
  "summary": { "total": 1, "passed": 1, "failed": 0, "errored": 0, "skipped": 0 },
  "cases": [
    {
      "id": "per-page-max",
      "target_id": "posts-list-query",
      "status": "passed",
      "duration_ms": 42
    }
  ]
}
```

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "wp_codebox_workloads": [
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
  leading `wp` token. The full bundled WP-CLI command surface is available —
  `wp plugin install --activate`, `wp theme install`, `wp option update`,
  `wp post create`, `wp eval`, etc. — the same set of built-in commands a
  user gets from the standalone `wp` phar. Use this when a workload needs to
  prepare WordPress.org plugin or theme dependencies before subsequent steps.

Workloads and steps may return `{ "metrics", "artifacts", "metadata" }`.
Numeric metrics are aggregated across measured iterations with the same
mean/p50/p95/p99/min/max suffixes used by PHP bench files. Artifacts and metadata
are carried into the Homeboy BenchResults scenario envelope.

### WordPress benchmark step-series artifacts

WordPress bench workloads that need row-level proof data can attach a generic
step-series artifact without changing top-level benchmark metrics. Return rows
under `metadata.step_series`; the WordPress bench artifact post-processor emits a
stable `series.json` next to `results.jsonl` and `leaderboard.md`:

```php
return [
    'metrics'  => [ 'transient_count' => 42 ],
    'metadata' => [
        'step_series' => [
            [
                'type'        => 'request',
                'label'       => 'GET /shop/',
                'url'         => '/shop/',
                'status'      => 'pass',
                'status_code' => 200,
                'elapsed_ms'  => 25.5,
                'metrics'     => [ 'db_queries' => 12 ],
                'metadata'    => [ 'cache_state' => 'cold' ],
            ],
            [
                'type'     => 'option_sample',
                'label'    => 'Layered nav transient count',
                'option'   => '_transient_wc_layered_nav_counts',
                'status'   => 'fail',
                'failure'  => [ 'message' => 'transient grew past budget' ],
                'metadata' => [ 'transient_count' => 42, 'budget' => 30 ],
            ],
        ],
    ],
];
```

`series.json` uses schema `homeboy/wordpress-bench-step-series/v1`:

```json
{
  "schema": "homeboy/wordpress-bench-step-series/v1",
  "component_id": "example-plugin",
  "generated_from": "homeboy/bench-results/v1",
  "series": [
    {
      "scenario_id": "layered-nav-cache",
      "label": "Layered nav cache",
      "source": "component",
      "artifact": null,
      "rows": [
        {
          "scenario_id": "layered-nav-cache",
          "index": 0,
          "type": "request",
          "label": "GET /shop/",
          "url": "/shop/",
          "status": "pass",
          "success": true,
          "status_code": 200,
          "elapsed_ms": 25.5,
          "metrics": { "db_queries": 12 },
          "metadata": { "cache_state": "cold" }
        }
      ]
    }
  ]
}
```

Recommended row fields:

- `type`: short category such as `request`, `crawl`, `option_sample`, `transient_sample`, or a domain-specific value.
- `label`: human-readable step label for reports.
- `elapsed_ms`: step elapsed time when applicable.
- `status` and `success`: row outcome. `status` values `pass`, `passed`, and `ok` normalize to `success: true`; `fail`, `failed`, and `error` normalize to `success: false`.
- `failure`: string or object with failure details.
- `metrics`: numeric or structured measurements for the row.
- `metadata`: arbitrary domain metadata, such as option keys, transient counts, cache state, request method, crawl depth, or fixture identifiers.

If a workload writes its own detailed step-series file, attach it predictably as
`artifacts.step_series` with `kind: "json"` and
`schema: "homeboy/wordpress-bench-step-series/v1"`. The post-processor preserves
that artifact reference in `series.json`, so Homeboy evidence and reporting can
discover either inline rows or workload-owned row files through the same key.

### WordPress benchmark state sampling helpers

Bench workloads that need to sample WordPress option or transient growth can use
the generic state sampling helper mounted with the WordPress extension:

```php
<?php
require_once '/homeboy-extension/scripts/bench/lib/wordpress-state-sampling.php';

$before = homeboy_wordpress_bench_sample_transient('example_cache', [
    'sample_index' => 0,
    'label' => 'before',
]);

// Run the workload operation that may change option or transient state.

$after = homeboy_wordpress_bench_sample_transient('example_cache', [
    'sample_index' => 1,
    'label' => 'after',
]);

return [
    'metadata' => [
        'state_samples' => [$before, $after],
        'state_delta' => homeboy_wordpress_bench_sample_delta($before, $after),
    ],
];
```

`homeboy_wordpress_bench_sample_option($name, $context)` samples a named option.
`homeboy_wordpress_bench_sample_transient($name, $context)` samples the backing
transient option row without workload SQL. Both helpers report existence/missing
state, serialized byte size, value type, array entry count when applicable, and
sample context such as `sample_index`, `label`, and `sampled_at_unix_ms`. Pass
`network => true` in the transient context to sample a site transient.

### WordPress benchmark WooCommerce fixture profiles

WooCommerce bench workloads can seed reusable store shapes by requiring the
WooCommerce fixture helper mounted with the WordPress extension:

```php
<?php
require_once '/homeboy-extension/scripts/bench/lib/woocommerce-fixtures.php';

return homeboy_wordpress_bench_wc_apply_fixture_profile(
    'small-shortcode-checkout',
    [
        'run_id' => getenv('HOMEBOY_RUN_ID') ?: 'local-checkout-run',
        'product_count' => 150,
    ]
);
```

The helper returns the normal workload payload shape with numeric `metrics` and
structured `metadata.woocommerce_fixture`. Fixture objects are scoped by a
normalized run id and deterministic prefix so repeated workloads can explain the
generated store shape in Homeboy artifacts.

Built-in profiles:

- `small-shortcode-checkout`: shortcode checkout, HPOS off, COD enabled, about
  150 products and 125 variations by default.
- `large-admin-catalog`: larger mixed virtual/physical catalog, categories,
  Woo admin/onboarding options, coupons, customers, and historical orders.
- `account-heavy-store`: many customers with repeat historical orders for account
  and login workloads.
- `shipping-package-matrix`: physical catalog with configurable shipping zones,
  methods, package count, and items per package metadata.

Common overrides include `run_id`, `product_count`, `variable_product_count`,
`variations_per_product`, `category_count`, `customer_count`,
`orders_per_customer`, `guest_order_count`, `coupon_count`,
`shipping_zone_count`, `shipping_methods_per_zone`, `hpos`, and `checkout`.
Profiles use WooCommerce APIs when WooCommerce is loaded; outside a WooCommerce
runtime the helper returns a structured `woocommerce_unavailable` failure instead
of fatalling, which keeps smoke tests and matrix diagnostics readable.

Playground grader workloads may also return a normalized reward payload:

```json
{
  "success": false,
  "reward": 0.75,
  "done": true,
  "grade": {
    "max_score": 1,
    "score": 0.75,
    "checks": [
      { "id": "valid_block_markup", "passed": true, "score": 0.4, "max_score": 0.4 },
      { "id": "matches_expected_structure", "passed": false, "score": 0, "max_score": 0.3 }
    ]
  }
}
```

`reward` is a finite number from `0` to `1`. `grade.score` and each check
`score` are finite numbers from `0` to their matching `max_score`. The runner
mirrors stable numeric keys into metrics (`success`, `reward`, `done`,
`grade_score`, and `grade_max_score`) so the normal BenchResults aggregation
emits fields such as `reward_mean` and `grade_score_mean`. The structured
payload is stored under `metadata.grade` with per-check `id`, `passed`, `score`,
`max_score`, and optional `message` fields.

Use `success` for binary task completion and `reward`/`grade.checks` when a
scenario can earn partial credit. Configured workload steps marked
`"role": "grader"` or `"grader": true` convert thrown exceptions into a
structured zero-reward grade with `metadata.grade.failure`, allowing result
aggregation to consume failures without scenario-specific parsing.

## Reusable Profiling Fixtures

Browser/API profiling workloads can seed a WordPress site before profiling by
calling the reusable fixture setup helper exported from
`wordpress/lib/page-profiler.js` or `wordpress/lib/fixture-setup.js`.

```js
const { profileWordPressPages } = require('./wordpress/lib/page-profiler');

await profileWordPressPages({
  page,
  baseUrl,
  manifest,
  sitePath,
  artifactDir,
  fixtures: [
    { id: 'scale-content', type: 'wp-eval-file', path: 'fixtures/scale.php' },
    { id: 'ready-flag', type: 'wp-cli', command: 'option update fixture_ready 1' }
  ]
});
```

For imperative setup, pass `setupWordPressFixture`:

```js
await profileWordPressPages({
  page,
  baseUrl,
  manifest,
  sitePath,
  artifactDir,
  async setupWordPressFixture({ runCli }) {
    await runCli('wp eval-file fixtures/scale.php');
  }
});
```

Supported declarative fixture step types:

- `wp-eval-file` with `path`: runs `wp eval-file <path>`.
- `wp-cli` with `command`: runs the command through WP-CLI. The command may
  include or omit the leading `wp` token.

Fixtures may declare `skipIf` or `idempotencyCheck` as a WP-CLI command. A zero
exit code skips that fixture step so already-seeded sites can be reused:

```json
{
  "id": "scale-content",
  "type": "wp-eval-file",
  "path": "fixtures/scale.php",
  "skipIf": "option get scale_fixture_ready"
}
```

The helper returns a `fixtureSetup` summary and writes
`wordpress-fixture-setup.json` when `artifactDir` is provided. Failed fixture
steps throw errors that include the fixture label, command, exit code, stdout,
and stderr.

Workloads that need temporary fixture plugins can use the plugin helpers from
`wordpress/lib/fixture-setup.js`:

```js
const {
	withWordPressFixturePlugins,
} = require('./wordpress/lib/fixture-setup');

await withWordPressFixturePlugins({
	sitePath,
	plugins: [
		{ path: '/path/to/plugin', plugin: 'plugin/plugin.php' },
		{ path: '/path/to/copied-plugin', copy: true, activate: false },
	],
	runCli: (command) => runCli(command, { cwd: sitePath }),
}, async (installedPlugins) => {
	// Run the profiling workload while plugins are mounted and activated.
});
```

The plugin helper backs up an existing plugin directory, installs each fixture
plugin by symlink or copy, activates entries unless `activate: false`, and
restores the original plugin tree in reverse order.

Fuzz and coverage recipe builders can also pass a product-agnostic fixture
profile. The WordPress extension maps the profile to WP Codebox
`inputs.siteSeeds`; WP Codebox owns the actual seed import and validation:

```json
{
  "fixture_profile": {
    "siteSeeds": [
      {
        "type": "fixture",
        "name": "generic-content",
        "source": "fixtures/content.json",
        "format": "json",
        "scopes": {
          "posts": { "slugs": ["home"] },
          "options": { "names": ["blogname"] }
        }
      }
    ]
  }
}
```

Profiles may also use a single seed object with top-level `posts`, `terms`,
`options`, `users`, `media`, `activePlugins`, or `activeTheme` keys; the mapper
collects those keys into the required `scopes` object.

## Reusable Block Quality Probes

WordPress workloads can collect product-neutral block quality counts without
copying PHP probe strings into each rig. Import the helpers from `wordpress` or
from `wordpress/lib/block-quality.js`:

```js
const {
	probeWordPressBlockQuality,
	probeWordPressPostBlockQuality,
} = require('./wordpress');

const siteQuality = await probeWordPressBlockQuality('/path/to/site', {
	postTypes: ['page', 'post', 'wp_template', 'wp_template_part'],
	postStatuses: ['any'],
});

const pageQuality = await probeWordPressPostBlockQuality('/path/to/site', 123);
```

The site probe reports stable WordPress counters:

```json
{
	"posts_seen": 4,
	"posts_with_content": 4,
	"posts_with_blocks": 3,
	"pages_seen": 1,
	"templates_seen": 1,
	"template_parts_seen": 1,
	"raw_html_unconverted": 1,
	"total_blocks": 18,
	"core_html_blocks": 2,
	"serialized_block_comments": 18,
	"fallback_count": 0,
	"core_html_without_fallback": 2,
	"post_type_counts": { "page": 1, "post": 1, "wp_template": 1, "wp_template_part": 1 }
}
```

The post-scoped probe includes the same counts plus `post_id`, `post_type`,
`post_title`, `stored_content_hash`, `stored_content_bytes`, and
`stored_content_preview`. Pass `contentPreviewBytes` to adjust the preview size.

Fallback counts are opt-in and product-neutral. If a workload owns a fallback
counter option, pass `fallbackOptionNames: ['example_fallback_count']`; the probe
sums those options into `fallback_count` and subtracts it from
`core_html_without_fallback`.

## Reusable Editor Canvas Probes

The WordPress extension also exports generic editor canvas helpers from
`wordpress/lib/editor-canvas-probes.js`:

```js
const {
	waitForWordPressEditorCanvas,
	captureWordPressEditorCanvasScreenshot,
	summarizeVisibleSelectors,
} = require('./wordpress');

await waitForWordPressEditorCanvas(page, {
	url: `${baseUrl}/wp-admin/site-editor.php`,
});

await captureWordPressEditorCanvasScreenshot(
	page,
	'artifacts/editor-canvas.png',
	{ url: `${baseUrl}/wp-admin/site-editor.php` }
);

const selectors = await summarizeVisibleSelectors(page, [
	{ name: 'hero', selectors: ['.hero', '.wp-block-cover'] },
	{ name: 'footer', selector: 'footer' },
]);
```

`waitForWordPressEditorCanvas()` waits for `iframe[name="editor-canvas"]`, then
waits inside the frame until `.block-editor-block-list__layout` has dimensions,
is not visibly loading, and contains at least one editor block. It applies a
small stabilizing stylesheet by default so screenshots are less noisy.

`summarizeVisibleSelectors()` is intentionally generic: it returns per-selector
match counts, visible counts, nonzero bounding-box counts, first-match text, and
group/totals summaries. Product-specific visual parity gates should stay in the
rig or workload that owns those expectations.

## WordPress/Codebox Visual Parity Workloads

WordPress benchmark workloads can import
`runWordPressCodeboxVisualParityWorkload()` from the WordPress extension when
they need to run a Codebox `wordpress.visual-compare` recipe and emit a
normalized `homeboy/VisualParityArtifact/v1` artifact.

```js
const { runWordPressCodeboxVisualParityWorkload } = await import('homeboy-extension-wordpress/wordpress-codebox-visual-parity-workload');

export default async function () {
  return runWordPressCodeboxVisualParityWorkload({
    id: 'homepage-parity',
    backend: { codeboxCli: process.env.CODEBOX_CLI },
    source: { path: './dist/site', label: 'static-source', port: 4173 },
    candidate: {
      url: '/',
      label: 'wordpress-candidate',
      recipe: { runtime: { wp: 'latest' }, inputs: { mounts: [] } },
    },
    viewport: { width: 1280, height: 1600 },
    threshold: 0.015,
  });
}
```

## Block Theme Quality Probe

Playground scenario graders can call a generic PHP-first WordPress quality probe
after the scenario action loop has modified the site. The helper is mounted with
the WordPress extension inside Playground:

```php
require_once '/homeboy-extension/scripts/bench/lib/block-theme-quality-probe.php';

return homeboy_wordpress_block_theme_quality_payload([
    'target_post_ids' => [(int) get_option('page_on_front', 0)],
]);
```

`homeboy_wordpress_collect_block_theme_quality()` returns the raw structured
probe. `homeboy_wordpress_block_theme_quality_payload()` wraps it as a Playground
workload payload: numeric and boolean values are emitted under `metrics`, and
the full raw probe is stored under `metadata.wordpress_quality`.

Collected signals include:

- active theme signals: `used_block_theme`, `theme_json_present`
- site/content counts: `front_page_id`, `pages_seen`, `templates_seen`,
  `template_parts_seen`, `navigation_posts_seen`
- block counts: `posts_with_blocks`, `total_blocks`, `core_html_blocks`,
  `serialized_block_comments`, `template_part_blocks`, `navigation_blocks`
- target/front-page counts: `target_pages_seen`, `target_posts_with_blocks`,
  `target_total_blocks`, `target_core_html_blocks`,
  `target_serialized_block_comments`
- fallback-quality signals: `raw_html_unconverted`,
  `target_raw_html_unconverted`, `navigation_created`

Use `target_post_ids` or `target_post_titles` when a scenario creates a specific
page that should be graded independently from the rest of the site. If no target
is supplied, the helper automatically treats `page_on_front` as the target when
that option is set.

Example grader that gives partial credit:

```php
require_once '/homeboy-extension/scripts/bench/lib/block-theme-quality-probe.php';

$quality = homeboy_wordpress_collect_block_theme_quality();
$checks = [
    [
        'id' => 'uses_block_theme',
        'passed' => $quality['used_block_theme'],
        'score' => $quality['used_block_theme'] ? 0.25 : 0,
        'max_score' => 0.25,
    ],
    [
        'id' => 'front_page_has_blocks',
        'passed' => $quality['target_total_blocks'] >= 5,
        'score' => $quality['target_total_blocks'] >= 5 ? 0.5 : 0,
        'max_score' => 0.5,
    ],
    [
        'id' => 'avoids_raw_html',
        'passed' => $quality['target_raw_html_unconverted'] === 0,
        'score' => $quality['target_raw_html_unconverted'] === 0 ? 0.25 : 0,
        'max_score' => 0.25,
    ],
];

$score = array_sum(array_column($checks, 'score'));

return [
    'success' => $score >= 1,
    'reward' => $score,
    'grade' => [
        'score' => $score,
        'max_score' => 1,
        'checks' => $checks,
    ],
    'metadata' => [
        'wordpress_quality' => $quality,
    ],
];
```

Playground bench runs also emit `wp-rl`-friendly artifacts next to the
BenchResults JSON file:

- `results.jsonl` — one JSON object per workload scenario row, excluding the
  synthetic `__bootstrap` BenchResults scenario. Rows include `scenario_id`,
  `provider`, `model`, `seed`, `run_id`, `success`, `reward`, `duration_ms`,
  `turns`, `tokens`, `artifacts`, and `error` when those values are present in
  scenario metadata, metrics, artifacts, or runner environment.
- `leaderboard.md` — a basic human summary grouped by provider/model with run
  count, success rate, error count, average reward, and average duration.

Rows tolerate partial and failed scenario envelopes. If a workload reports
`metadata.provider`, `metadata.model`, `metadata.seed`, `metadata.tokens`,
`metrics.reward_mean`, `metrics.success_mean`, `metrics.turns_mean`, or an
`error`/`failure` object, those fields are projected directly into
`results.jsonl` for downstream analysis without custom post-processing.

Example `results.jsonl` row:

```json
{"component_id":"example-plugin","scenario_id":"block-markup/navigation-001","provider":"openai","model":"gpt-5.5","seed":1,"run_id":"1","success":true,"reward":1,"duration_ms":1234,"turns":7,"tokens":{"input":1000,"output":500},"artifacts":{"transcript":{"path":"artifacts/transcript.json","kind":"json"}},"error":null}
```

Set `HOMEBOY_PLAYGROUND_RESULTS_ARTIFACT_DIR` to write these derived artifacts
to a specific directory. Otherwise they are written beside
`HOMEBOY_BENCH_RESULTS_FILE`.

The same workload contract powers runtime-backed agent tasks on the WP Codebox
WordPress substrate. See `.github/workflows/runtime-agent-full-run.yml` for the
reusable runtime agent workflow contract.

## WP Codebox Validation Profile

The WordPress extension declares a `wp-codebox-validation` CI profile for
reviewer-facing validation of WP Codebox changes. The profile keeps cheap,
non-benchmark checks local and routes benchmark work through Homeboy Lab/runner
offload so controller machines do not execute benchmark workloads.

Reviewer rerun sequence:

```bash
homeboy build <component>
homeboy test <component> --ci-job wp-codebox-phpunit
homeboy config set /bench/local_execution '"denied"'
homeboy bench <component> --ci-profile wp-codebox-validation-bench --runner <runner-id>
```

Use `wp-codebox-validation-bench` for `homeboy bench --ci-profile` because the
generic bench command requires exactly one bench job. Keep the full
`wp-codebox-validation` profile as the human-readable checklist that ties local
smoke coverage, offloaded benchmarks, durable artifacts, and rerun commands
together.

Evidence expectations:

- Local checks may include build/package validation and the WP Codebox PHPUnit
  smoke runner.
- Benchmark evidence must come from a connected Homeboy Lab/runner run. Set
  `/bench/local_execution` to `denied` first so missing runner/offload setup
  fails closed instead of falling back to local execution.
- Reviewer-facing summaries should link the Homeboy run/artifact bundle and
  include the rerun commands above. Do not cite machine-local paths as PR
  evidence.

### Nested Plugin Source Roots

Runtime bench runs normally treat the selected Homeboy component path as the
plugin source. Monorepos can keep that component path scoped to the nested
plugin while asking the runner to materialize a broader checkout for host-side
prep and Composer path repositories. These source-root settings are legacy
WP Codebox bench aliases until the bench recipe generator grows generic names:

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "wp_codebox_source_root": "/path/to/monorepo",
        "wp_codebox_source_subpath": "plugins/example-plugin"
      }
    }
  }
}
```

For Lab offload, pass legacy `wp_codebox_source_root` as a path-valued
`--setting` when the root must be synced separately from the selected component
snapshot. Homeboy remaps that setting to the runner path before the WordPress
bench runner starts.
The bench runner keeps `HOMEBOY_COMPONENT_PATH` and the plugin slug scoped to the
selected component, but uses the configured source root/subpath for prepare step
cwd resolution, workload discovery, file mounts, and the WP Codebox plugin input.

## WP Codebox Scenario Manifests

Repos can declare first-class scenario manifests and let the WordPress runner
compile them into legacy `wp_codebox_workloads`. This keeps eval/RL-style
scenarios on the WP Codebox recipe execution path instead of adding a second
runner.

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "wp_codebox_scenario_manifests": [
          "scenarios/navigation-001.json"
        ]
      }
    }
  }
}
```

Manifest shape:

```json
{
  "id": "block-markup/navigation-001",
  "label": "Generate valid navigation block markup",
  "prompt_file": "prompt.md",
  "blueprint": "blueprints/navigation-001.json",
  "grader": "graders/navigation-001.php",
  "tags": ["blocks", "markup", "medium"],
  "limits": {
    "max_turns": 8,
    "step_budget": 12,
    "time_budget_ms": 600000
  },
  "run": [
    { "type": "php", "file": "workloads/run-agent.php" }
  ],
  "metadata": {
    "corpus": "wp-rl-smoke"
  }
}
```

Supported fields:

- `prompt` or `prompt_file`: prompt text is copied into scenario metadata. File
  references resolve relative to the manifest file.
- `blueprint` or `blueprint_file`: inline object or JSON file passed to
  WP Codebox as part of the generated recipe runtime blueprint.
- `run`: existing legacy `wp_codebox_workloads` steps for the model or agent
  action loop. The supported step types are still `php`, `ability`, and
  `wp-cli`.
- `grader` or `grader_file`: PHP file appended after `run`, so grading happens
  after the action loop.
- `rules`, `general_rules`, `task_rules`, and `probes`: copied into scenario
  metadata so eval corpora can declare reusable policy and zero-weight
  behavioral probes separately from grader reward math.
- `tags`, `metadata`, and `limits`: copied into the BenchResults scenario
  envelope for reports, filtering, and downstream eval tooling.

Runtime agent workloads also evaluate known general rules against available
runner evidence and expose the results under
`metadata.eval_artifact.general_rule_results`. Initial executable general rules
cover editable block failures, raw HTML/shortcode failures, speculative plugin
packaging metadata, unsupported plugin author metadata, docs-standards failures
when evidence is attached, and production-build parity when buildable asset paths
changed.

Relative manifest entries resolve from the component/corpus root. Relative
references inside a manifest resolve from the manifest file's directory. Inline
manifest objects resolve relative paths from the component root.

Example: drive a plugin's pipeline through an Abilities API entry point.

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "wp_codebox_blueprint": {
          "steps": [
            { "step": "installPlugin", "pluginData": { "resource": "wordpress.org/plugins", "slug": "example-plugin" } }
          ]
        },
        "wp_codebox_workloads": [
          {
            "id": "smoke-pipeline",
            "run": [
              {
                "type": "ability",
                "ability": "example/run-pipeline",
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

## External HTTP Guardrail Helper

The WordPress extension exports a generic Node helper for bench and request
workloads that need deterministic control over outbound WordPress HTTP calls. It
installs a temporary MU-plugin that observes `pre_http_request`, writes sanitized
JSONL events, and can return a deterministic WordPress HTTP API response before
network I/O occurs.

```js
const {
  installWordPressExternalHttpGuardrail,
  collectWordPressExternalHttpGuardrailEvents,
  summarizeWordPressExternalHttpGuardrailEvents,
  uninstallWordPressExternalHttpGuardrail,
} = require('homeboy-extension-wordpress/external-http-guardrail');

const sitePath = '/path/to/wordpress';

installWordPressExternalHttpGuardrail(sitePath, {
  allowlistDomains: ['api.wordpress.org', 'example.test'],
  blockResponse: {
    code: 599,
    message: 'External HTTP blocked by Homeboy guardrail',
    body: '',
  },
});

// Run one or more browser, curl, WP-CLI, bench, or trace requests here.

const events = collectWordPressExternalHttpGuardrailEvents(sitePath);
const summary = summarizeWordPressExternalHttpGuardrailEvents(events);
uninstallWordPressExternalHttpGuardrail(sitePath);

console.log(summary.hosts);
```

Policies are generic WordPress HTTP policies:

- `allowlistDomains` — exact host names or parent domains. `example.test` allows
  `example.test` and `api.example.test`.
- `blockNetwork` — when omitted, defaults to `true` if an allowlist is present
  and `false` otherwise. Set `blockNetwork: true` to block every non-allowlisted
  outbound call, or `false` to observe only.
- `blockResponse` — deterministic response returned from `pre_http_request` for
  blocked calls. Defaults to status `599`, an explanatory message, and an empty
  body.
- `redactUrls` — defaults to `true`. Stored event URLs preserve scheme, host,
  port, and path while replacing query strings with `?redacted=1`, redacting URL
  user info, and dropping fragments.

By default the helper writes `wp-content/homeboy-external-http.jsonl` and
installs `wp-content/mu-plugins/homeboy-external-http-guardrail.php`. The JSONL
file is left in place during cleanup so benchmark and trace runners can preserve
it as an artifact. Pass `{ removeArtifact: true }` to
`uninstallWordPressExternalHttpGuardrail` when the raw event log should also be
deleted.

Captured entries use `http.allowed` and `http.blocked` events with host, method,
redacted URL, hashed request ID, and block decision fields. The summary helper
returns total allowed/blocked counts, per-host counts, and redacted samples for
reviewable artifacts.

## WordPress Helper Discovery For Node Workloads

Node.js rigs and bench workloads should discover WordPress helper files through
the helper manifest instead of hardcoding local checkout paths. The WordPress
extension exports the manifest at `homeboy-extension-wordpress/helper-manifest`:

```js
const {
	getWordPressHelperManifest,
} = require('homeboy-extension-wordpress/helper-manifest');

const manifest = getWordPressHelperManifest();
const requestProfiler = require(manifest.helpers.requestProfiler);
```

The manifest contract is versioned and currently exposes absolute paths for:

- `helpers.requestProfiler` — `wordpress/lib/request-profiler.js`
- `helpers.externalHttpGuardrail` — `wordpress/lib/external-http-guardrail.js`
- `helpers.timingCorrelator` — `wordpress/lib/timing-correlator.js`
- `helpers.bootstrapTimeline` — `wordpress/lib/wordpress-bootstrap-timeline.js`

When the Node.js bench runner is running from the standard Homeboy extensions
checkout and the sibling WordPress extension is present, it also exports these
stable environment variables for rig-owned extra workloads:

- `HOMEBOY_WORDPRESS_HELPER_MANIFEST`
- `HOMEBOY_WORDPRESS_REQUEST_PROFILER_HELPER`
- `HOMEBOY_WORDPRESS_TIMING_CORRELATOR_HELPER`
- `HOMEBOY_WORDPRESS_BOOTSTRAP_TIMELINE_HELPER`

Example for a rig-owned Node benchmark workload:

```js
const manifestPath = process.env.HOMEBOY_WORDPRESS_HELPER_MANIFEST;
const { getWordPressHelperManifest } = await import(manifestPath);

const { installWordPressRequestProfiler } = await import(
	getWordPressHelperManifest().helpers.requestProfiler
);
```
