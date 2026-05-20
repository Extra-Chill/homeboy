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
bootstrap, `playground_blueprint`, dependency mounts, and component load through
a generated WP Codebox recipe.

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
  leading `wp` token. The full bundled WP-CLI command surface is available —
  `wp plugin install --activate`, `wp theme install`, `wp option update`,
  `wp post create`, `wp eval`, etc. — the same set of built-in commands a
  user gets from the standalone `wp` phar. Use this when a workload needs to
  prepare WordPress.org plugin or theme dependencies before subsequent steps.

Workloads and steps may return `{ "metrics", "artifacts", "metadata" }`.
Numeric metrics are aggregated across measured iterations with the same
mean/p50/p95/p99/min/max suffixes used by PHP bench files. Artifacts and metadata
are carried into the Homeboy BenchResults scenario envelope.

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

The same workload contract powers Data Machine agent CI on the WP Codebox
WordPress substrate. See
[`../../wordpress/docs/AGENT_CI_WP_CODEBOX.md`](../../wordpress/docs/AGENT_CI_WP_CODEBOX.md)
for the dedicated agent sandbox guide.

## Playground Scenario Manifests

Repos can declare first-class scenario manifests and let the WordPress runner
compile them into `playground_workloads`. This keeps eval/RL-style scenarios on
the WP Codebox recipe execution path instead of adding a second runner.

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "playground_scenario_manifests": [
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
- `run`: existing `playground_workloads` steps for the model or agent action
  loop. The supported step types are still `php`, `ability`, and `wp-cli`.
- `grader` or `grader_file`: PHP file appended after `run`, so grading happens
  after the action loop.
- `rules`, `general_rules`, `task_rules`, and `probes`: copied into scenario
  metadata so eval corpora can declare reusable policy and zero-weight
  behavioral probes separately from grader reward math.
- `tags`, `metadata`, and `limits`: copied into the BenchResults scenario
  envelope for reports, filtering, and downstream eval tooling.

Data Machine agent workloads also evaluate known general rules against available
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
