# Homeboy WordPress Extension

Homeboy extension that gives WordPress plugins and themes a complete
`test → lint → build → bench → trace → audit` pipeline with zero in-component
configuration. PHPUnit and benchmark workloads run through [WP Codebox][wp-codebox]
by default, so there is no host PHP, MySQL, or local WordPress install to manage.

[wp-codebox]: https://github.com/chubes4/wp-codebox
[playground]: https://playground.wordpress.net/

## What this extension provides

It registers the `wordpress` component kind with Homeboy core and wires up
extension scripts for these verbs:

| Homeboy verb | What it does | Entry script |
|---|---|---|
| `test` | PHPUnit via WP Codebox (default) or host-PHP smoke scripts | `scripts/test/test-runner.sh` |
| `lint` | PHPCS + PHPStan (PHP) and ESLint (JS/TS) | `scripts/lint/lint-runner.sh` |
| `build` | Production ZIP with composer `--no-dev`, asset build, syntax check | `scripts/build/build.sh` |
| `bench` | Benchmark workloads via WP Codebox; optional browser handoff | `scripts/bench/bench-runner.sh` |
| `trace` | Project-owned scenario traces | `scripts/trace/trace-runner.sh` |
| `audit` | Detector rules over PHP for lifecycle / role tagging | `scripts/audit/setup-references.sh` + rules in `wordpress.json` |
| `fingerprint` | File-shape fingerprinting for change detection | `scripts/fingerprint.sh` |
| `refactor` | Auto-fix pass (PHPCBF + custom fixers) | `scripts/refactor.py` |
| `crossref` | Cross-reference analysis across sources/tests | `scripts/test/crossref.php` |
| `validate` | PHP syntax / PSR-4 / dependency validation | `scripts/validation/validate-syntax.sh` |
| `format` | Post-write formatting | `scripts/format.sh` |

It also declares a WordPress platform integration in `wordpress.json`:
WP-CLI database query templates, default pinned files (`wp-config.php`,
`.htaccess`, `robots.txt`), debug log paths, table groupings, and a discovery
command that scans for `wp-config.php` to find sites.

## Quick start

```bash
# Run PHPUnit + lint for a single component
homeboy test <component-id>

# Lint only (PHPCS + PHPStan + ESLint where applicable)
homeboy lint <component-id>

# Apply lint auto-fixes (PHPCBF + custom fixers)
homeboy refactor <component-id>

# Production ZIP at <component>/build/<component-id>.zip
homeboy build <component-id>

# Benchmark configured workloads
homeboy bench <component-id>

# Run a scenario trace
homeboy trace <component-id> --scenario <name>

# Audit detector pass
homeboy audit <component-id>
```

`<component-id>` matches the id Homeboy core uses for the component. Most
verbs also accept a project id to fan out across all of its components.

## Test runner

The default backend boots WordPress through WP Codebox, mounts the component
under `/wordpress/wp-content/plugins/<slug>` (or themes path for themes), and
runs PHPUnit in-process. No `bootstrap.php` or `phpunit.xml` in the component is
required — and **shipping one is rejected** with a clear error. The extension
owns bootstrap.

A component needs:

- `tests/` directory with PHPUnit tests (default discovery: `*Test.php`
  suffix or `test-*` prefix, recursive).
- A plugin header (`Plugin Name:`) or theme `style.css` with `Theme Name:`
  — the runner detects which is which.

The test runner emits a structured log at
`<component>/.pg-test-result.txt` with `STAGE_BEGIN` / `STAGE_OK` /
`STAGE_FAIL` / `STAGE_FATAL` / `NOTICE` markers across stages
`boot → install → load_fixtures → load_deps → load_component → discover_tests
→ load_tests → run_tests`. The bash runner parses these to classify
failures.

### Writing tests

```php
<?php
// tests/test-my-feature.php

class Test_My_Feature extends WP_UnitTestCase {

    public function test_post_creation() {
        $post_id = self::factory()->post->create([
            'post_title'  => 'Test Post',
            'post_status' => 'publish',
        ]);
        $this->assertEquals( 'Test Post', get_the_title( $post_id ) );
    }
}
```

Available factories from the WordPress test framework: `user`, `post`,
`comment`, `term`, `category`, `tag`, `attachment`.

### Host-smoke backend

Pure PHP smoke suites that don't need WordPress can opt out of WP Codebox:

```bash
homeboy component set <component-id> test_backend host-smoke
```

The host-smoke backend discovers `tests/**/*-smoke.php`, runs each script
in its own host `php` process, emits `HOST_SMOKE_*` markers, and fails
fast with the failing script name. It does not bootstrap WordPress.

### Runtime dependencies

If a plugin depends on other local plugins at runtime, declare them via
`validation_dependencies`. They are mounted alongside the plugin under
test and loaded during the `load_deps` bootstrap stage:

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

### `db.php` drop-ins

Plugins that ship a `db.php` drop-in are supported automatically. The
runner mounts `<plugin>/db.php` to `/wordpress/wp-content/db.php`;
Playground's built-in SQLite mu-plugin detects it and steps aside. See
[`docs/PLAYGROUND_DROPIN.md`](docs/PLAYGROUND_DROPIN.md) for the
coexistence mechanism.

For everything else about testing — debug markers, level overrides,
phpunit.xml consumption, current WP Codebox parity status, known gaps — see
[`docs/TESTING.md`](docs/TESTING.md).

## Lint runner

Lint runs before PHPUnit, and a lint failure aborts the test run with
exit code 1. Files are routed by extension:

| Extensions | Steps |
|---|---|
| `.php` | `phpcs`, `phpstan` |
| `.js` `.jsx` `.ts` `.tsx` | `eslint` |

### PHPCS

`phpcs.xml.dist` applies WordPress Coding Standards with PSR-4
adjustments. Text domain is auto-detected from the plugin header. When
PHPCS reports auto-fixable findings, the runner surfaces a CTA showing
the exact `homeboy refactor` command to clean them up.

### PHPStan

`phpstan.neon.dist` runs at **level 7** with WordPress + WP-CLI +
WooCommerce stubs. Level 7 unlocks argument-type flow analysis (catches
`false` / `null` leaking into strict-typed callees). `missingType.*`
identifiers are suppressed by default to keep the signal-to-noise ratio
high. Tests are analyzed alongside source.

Components with pre-existing findings can capture a baseline:

```bash
path/to/extension/vendor/bin/phpstan analyse \
    --configuration=path/to/extension/phpstan.neon.dist \
    --level=7 --memory-limit=2G \
    --generate-baseline=phpstan-baseline.neon \
    .
```

Commit `phpstan-baseline.neon` at the component root. The runner detects
it and pulls it in via `includes:`. New code must not add new findings;
delete the baseline to ratchet toward full cleanup.

Knobs:

- `HOMEBOY_PHPSTAN_LEVEL=8 homeboy test <component>` — bump one-off.
- `HOMEBOY_SKIP_PHPSTAN=1` — critical-only check that still blocks
  `function.notFound` / `class.notFound` (guaranteed runtime fatals).

### Custom sniff: multi-line comment style (opt-in)

`HomeboyWordPress.Commenting.MultiLineInlineComment` enforces
[WP Inline Documentation Standards §5.2][wp-docs-5.2] — flags 2+ adjacent
`//` lines that should be a `/* ... */` block, and `/**` blocks used for
prose instead of declarations. Auto-fixable. Registered but off by default.
Opt in by promoting the rule severity in your project ruleset, or run on
demand:

```bash
vendor/bin/phpcs --sniffs=HomeboyWordPress.Commenting.MultiLineInlineComment src/
```

[wp-docs-5.2]: https://developer.wordpress.org/coding-standards/inline-documentation-standards/php/#5-2-multi-line-comments

### ESLint

WordPress ESLint config. Skipped automatically when no JS/JSX/TS/TSX
files exist in the component. Components must not ship local
`.eslintrc` — the extension owns the standards.

### Sanctioned suppressions

Prefer fixing real findings. When a finding is caused by a deliberately
loose WordPress/runtime boundary, use a narrow suppression on the line
immediately before the finding with a runtime-contract justification.
See [`docs/TESTING.md`](docs/TESTING.md#sanctioned-lint-suppressions) for
canonical examples (defensive public API guards, redirect-result reads,
minimum-PHP runtime guards).

## Build runner

```text
homeboy build <component>
    │
    ├─ Detect plugin/theme from headers (Plugin Name | Theme Name)
    ├─ Extract version
    ├─ Stage into .homeboy-build/  (avoids @wordpress/scripts build/ collision)
    ├─ Install production deps   (composer --no-dev, npm ci if applicable)
    ├─ Build frontend assets     (@wordpress/scripts when present)
    ├─ Copy files                (rsync, respects .buildignore)
    ├─ Validate build structure  (php -l, PSR-4)
    ├─ ZIP → build/<component-id>.zip
    └─ Restore dev dependencies
```

Pre-build validation runs `scripts/build/validate-build.sh`. PHP syntax
errors and PSR-4 violations block the build; lint findings do not.

## Bench runner

Bench workloads run through WP Codebox. WP Codebox owns the disposable WordPress
runtime, component mount, command execution, and artifacts, and emits the same
`BenchResults` envelope Homeboy core parses.

Each file under `tests/bench/*.php` returns a callable. The callable may return
numeric metrics directly or `{metrics, metadata, artifacts}`. Components may
also declare configured workloads via `wp_codebox_workloads`; the WordPress
runner maps those declarations into a temporary WP Codebox recipe alongside
`validation_dependencies`, `wp_codebox_file_mounts`, `wp_config_defines`,
`bench_env`, shared-state mounts, and browser handoff descriptors.

The generated recipe is the single WP Codebox entry point for benchmarks:
`wp_codebox_blueprint` becomes `runtime.blueprint`, dependencies become recipe
plugin inputs, and scenario manifests compile into configured workloads.

The browser bench target is a two-extension handoff: the WordPress
extension prepares/describes the WordPress target by writing
`${HOMEBOY_BENCH_SHARED_STATE}/browser-target.json`; a Node-side browser
helper owns Playwright, browser metrics, screenshots, and browser
artifacts. The raw target file is a handoff artifact and may contain
credentials — runners must not publish it without redacting fields
listed in `artifactPolicy.secretFields`. Full schema in
[`docs/TESTING.md`](docs/TESTING.md#browser-bench-target-handoff).

### Page profiler primitives

The extension exports reusable Node helpers for browser-backed WordPress page
profiling. Rigs keep ownership of site lifecycle and Playwright launch, then
call `profileWordPressPage()` or `profileWordPressPages()` with a manifest of
URLs and readiness gates. The helpers collect resource timings, classify REST /
admin / asset waterfalls, and correlate browser timings with rows from the
temporary request profiler.

```js
const {
	installWordPressRequestProfiler,
	collectWordPressRequestProfiles,
	profileWordPressPages,
} = require('homeboy-extension-wordpress');

installWordPressRequestProfiler(sitePath, { clearArtifact: true });

const result = await profileWordPressPages({
	page,
	baseUrl: status.siteUrl,
	manifest: {
		pages: [
			{ id: 'home', path: '/', ready: { state: 'networkidle' } },
			{ id: 'dashboard', path: '/wp-admin/index.php', ready: '#dashboard-widgets' },
			{
				id: 'site-editor',
				path: '/wp-admin/site-editor.php',
				ready: {
					selector: 'iframe[name="editor-canvas"]',
					frameName: 'editor-canvas',
					frameSelector: '[data-block]',
				},
			},
		],
	},
	wordpressProfilerRows: collectWordPressRequestProfiles(sitePath),
});
```

For common wp-admin and Site Editor profiling, use the documented admin page
scenario catalog instead of copying selectors or iframe readiness checks into a
rig. Built-in profiling scenarios include `dashboard`, `add-themes`, and
`site-editor-root`; custom scenario objects use the same shape as page profiler
manifest entries.

```js
const {
	collectWordPressRequestProfiles,
	profileWordPressAdminPageScenario,
} = require('homeboy-extension-wordpress');

// Or import only the scenario helpers:
// const { profileWordPressAdminPageScenario } = require('homeboy-extension-wordpress/admin-page-scenarios');

await profileWordPressAdminPageScenario({
	page,
	siteUrl: status.siteUrl,
	autoLoginUrl: status.autoLoginUrl,
	scenario: 'site-editor-root',
	mark,
	wordpressProfilerRows: collectWordPressRequestProfiles(sitePath),
});
```

`profileWordPressAdminPageScenarios()` accepts `scenarios` with built-in IDs or
custom objects, returns page profiles plus stable `metrics` and `metadata.summary`,
and composes with the Node.js browser benchmark helper by using its provided
`page` and `mark` arguments inside the benchmark action.

This is intentionally extension-level infrastructure, not Homeboy core. Core
keeps orchestration, run persistence, baselines, and reports; WordPress owns
WP-specific browser readiness, REST route classification, request profiling,
and URL manifests. If the same page-profiling primitive appears in another
extension later, then the shared seam can be promoted into core.

### Bootstrap timeline primitives

The extension also exports reusable helpers for temporary WordPress bootstrap
timeline probes. Rigs own workload orchestration and benchmark thresholds; the
WordPress extension owns the generic probe install, JSONL collection, summary,
and cleanup behavior for `index.php` and `wp-settings.php`.

```js
const {
	installWordPressBootstrapTimeline,
	collectWordPressBootstrapTimeline,
	summarizeWordPressBootstrapTimeline,
	uninstallWordPressBootstrapTimeline,
} = require('homeboy-extension-wordpress/wordpress-bootstrap-timeline');

installWordPressBootstrapTimeline(sitePath, { clearArtifact: true });

try {
	// Run the rig-owned workload that requests the WordPress site.
	const rows = collectWordPressBootstrapTimeline(sitePath);
	const summary = summarizeWordPressBootstrapTimeline(rows, { limit: 20 });
	console.log(summary);
} finally {
	uninstallWordPressBootstrapTimeline(sitePath);
}
```

Install creates backups under
`wp-content/homeboy-bootstrap-timeline-backups/`, writes timing rows to
`wp-content/homeboy-bootstrap-timeline.jsonl`, and restores the original core
entry files on uninstall. Uninstall preserves the artifact by default so callers
can collect it after cleanup; pass `{ removeArtifact: true }` when the caller
owns artifact retention elsewhere.

## Trace runner

Project-owned scenarios live under one of:

- `traces/<scenario>.trace.php`
- `tests/traces/<scenario>.trace.php`
- `scripts/trace/<scenario>.sh`

Run with `homeboy trace <component-id> --scenario <name>`.

## Audit runner

`audit` applies the detector rule sets declared under the `audit` block
of `wordpress.json` — lifecycle path globs, utility-suffix conventions,
convention-exception globs, and convention tag globs (e.g. tagging
`bootstrap.php` / `register-*.php` / `*-functions.php` as
`wordpress:php-role:procedural-helper`). The extension's own smoke
scripts under `tests/audit-*-smoke.sh` cover regressions in the rule set.

### WP Codebox audit fan-out

The canonical Homeboy mutation path is `homeboy refactor --from audit --write`;
`audit` remains read-only. WordPress-owned WP Codebox fan-out helpers live under
`scripts/agent/` so extension orchestration can turn a structured audit report
into sandbox task requests without teaching Homeboy core about WordPress or WP
Codebox.

When Homeboy's generic extension refactor-source seam is enabled, the WordPress
extension handles the audit source with:

```bash
homeboy refactor <component> \
  --from audit \
  --setting refactor.audit.extension=wordpress \
  --setting wp_codebox_provider=opencode \
  --setting wp_codebox_model=opencode-go/kimi-k2.6 \
  --setting wp_codebox_provider_plugin_paths=/Users/chubes/Developer/ai-provider-for-opencode \
  --setting wp_codebox_secret_env=OPENCODE_API_KEY
```

`scripts/agent/homeboy-audit-wp-codebox-fanout.cjs` turns a structured audit
report into one `homeboy/wp-codebox-task-request/v1` request per fix batch. With
`--execute`, it streams each request to a WP Codebox task runner command such as
`scripts/agent/homeboy-wp-codebox-task-runner.cjs`, which builds a
provider-configured `wp-codebox/workspace-recipe/v1` recipe.

### WP Codebox agent-task executor

The WordPress extension declares `wordpress.codebox-agent-task-executor` in
`wordpress.json` under `agent_task_executors`. The provider contract advertises a
Codebox backend with browser runtime, WordPress sandbox, artifact materialization,
screenshots, and structured outcome capabilities without adding Codebox imports or
WordPress assumptions to Homeboy core.

`scripts/agent/homeboy-codebox-agent-task-executor.cjs` accepts a generic
`homeboy/agent-task-request/v1` request, maps it into the existing WP Codebox task
runner request, invokes the Codebox recipe runner, and emits a
`homeboy/agent-task-outcome/v1` outcome with normalized status, artifacts,
evidence refs, diagnostics, and failure classification.

The richer sandbox-agent host-tool registration surface is blocked on
https://github.com/chubes4/wp-codebox/issues/392. Until that lands, this provider
is a preparatory contract plus request/outcome adapter around the current
`wp-codebox.agent-sandbox-run` command boundary.

Approved artifact-map entries become `apply_back` records for the reviewed
apply adapter. Rejected entries with `approved: false` become `issue_reports`
records instead, preserving the finding IDs, artifact evidence, disposition,
reason, and issue title/body metadata needed to file a follow-up false-positive
or rejected-artifact tracker.

## Component settings

Configure per-component in the component's homeboy/component config under
`extensions.wordpress.settings`. All settings have safe defaults.

| Setting | Type | Default | Purpose |
|---|---|---|---|
| `test_backend` | string | `wp-codebox` | `wp-codebox` (default) or `host-smoke` for standalone non-WordPress smoke scripts |
| `validation_dependencies` | string | `""` | Comma / newline / JSON list of local components to mount during PHPStan, autoload validation, and PHPUnit |
| `user` | string | `""` | WP-CLI user (email/login/ID); appended as `--user` when set |
| `wp_config_defines` | object | `{}` | `CONSTANT_NAME => value` map appended to the runtime `wp-tests-config.php`; PHP type preserved via `var_export` |
| `bench_env` | object | `{}` | `NAME => value` env vars forwarded into the runtime (workloads/fixtures read via `getenv()`) |
| `wp_codebox_blueprint` | object | `{}` | Runtime blueprint merged into the generated WP Codebox bench recipe |
| `wp_codebox_workloads` | array | `[]` | Declared bench workloads passed to `wordpress.bench` through the generated recipe after deps and component load |
| `wp_codebox_file_mounts` | array | `[]` | Files from the component or validation dependencies mounted into explicit WordPress runtime paths |
| `bench_browser_target` | object | `{}` | Browser bench target descriptor (see Bench runner above) |

## Blueprint Validation

Use `scripts/validation/validate-playground-blueprint.sh` to validate the same
Blueprint file or URL users open in WordPress Playground:

```bash
wordpress/scripts/validation/validate-playground-blueprint.sh \
  https://raw.githubusercontent.com/example/repo/main/blueprint.json
```

The script runs `wp-codebox validate-blueprint` and prints captured stdout/stderr
on failure, including step-level Blueprint errors and PHP fatals surfaced by the
WP Codebox Playground runtime. Set `HOMEBOY_WP_CODEBOX_BIN` to validate with a
specific wp-codebox binary.

## WP Codebox Doctor

Use `scripts/doctor/wp-codebox-doctor.sh` when the WordPress runner looks stuck
or the Playground runtime cache may be bad:

```bash
wordpress/scripts/doctor/wp-codebox-doctor.sh doctor
wordpress/scripts/doctor/wp-codebox-doctor.sh cleanup --stale-after-seconds 3600
```

The doctor reports the configured `wp-codebox` binary/source SHA, Node/npm
availability, stale `recipe-run` processes, and corrupt `.zip` files in known WP
Codebox/WordPress Playground cache roots. `cleanup` sends `TERM` to stale
recipe-run processes and removes corrupt archives so the next runner invocation
rebuilds them.

## Environment variables

| Variable | Purpose |
|---|---|
| `HOMEBOY_DEBUG=1` | Verbose runner diagnostics |
| `HOMEBOY_SKIP_LINT=1` | Skip PHPCS / ESLint (does not skip PHPStan) |
| `HOMEBOY_SKIP_TESTS=1` | Skip PHPUnit |
| `HOMEBOY_SKIP_PHPSTAN=1` | Critical-only PHPStan (still blocks runtime-fatal identifiers) |
| `HOMEBOY_PHPSTAN_LEVEL=N` | One-off PHPStan level override |
| `HOMEBOY_FIX_ONLY=1` | Lint runner fix-only mode (set automatically by `homeboy refactor`) |
| `HOMEBOY_SUMMARY_MODE=1` | Compact summary output |
| `HOMEBOY_STEP=phpcs` / `HOMEBOY_SKIP=eslint` | Filter lint steps |

## Blocking vs advisory

| Check | Behavior |
|---|---|
| PHPCS | **Blocks** the test run (lint-before-tests gate) |
| PHPStan (level 7) | **Blocks** |
| ESLint | **Blocks** when JS/TS files are present |
| PHPUnit (Playground or host-smoke) | **Blocks** |
| `php -l` syntax check (build) | **Blocks** |
| PSR-4 validation (build) | **Blocks** |
| Audit detector findings | Advisory by default (depends on rule severity) |

## Repository layout

```text
wordpress/
├── HomeboyWordPress/         # Custom PHPCS sniff(s) + ruleset
│   ├── Sniffs/
│   ├── Tests/
│   └── ruleset.xml
├── docs/
│   ├── CHANGELOG.md
│   ├── TESTING.md            # Canonical test/lint/bench reference
│   ├── PLAYGROUND_DROPIN.md  # db.php coexistence mechanism
│   └── commands/
├── lib/                      # Node helpers: request profiler, Playground HTTP readiness
├── scripts/
│   ├── audit/                # Detector setup + WP test smells
│   ├── bench/                # WP Codebox bench runner + result artifact helpers
│   ├── build/                # build.sh, validate-build.sh, validate-psr4.sh
│   ├── env/detect.sh         # component_env detector
│   ├── lib/                  # Shared runner helpers
│   ├── lint/                 # lint-runner.sh, eslint-runner.sh, phpstan-runner.sh
│   ├── test/                 # test-runner*.sh, parsers, smokes
│   ├── trace/                # trace-runner.sh
│   ├── validation/           # syntax / PSR-4 / dependency validators
│   ├── fingerprint.sh
│   ├── format.sh
│   └── refactor.py
├── stubs/
│   └── wordpress-api-overrides.stub.php
├── tests/                    # Extension's own smoke tests (NOT plugin tests)
├── composer.json             # PHP toolchain (PHPUnit 9, PHPCS, PHPStan 2, wp-phpunit, etc.)
├── package.json              # Node toolchain (@wordpress/eslint-plugin, eslint)
├── homeboy.json              # Self-checks (lint + smoke matrix)
├── phpcs.xml.dist
├── phpstan.neon.dist
├── phpunit.xml.dist
└── wordpress.json            # Extension manifest: verbs, settings, platform integration
```

## Requirements

- **bash 4.0+** (macOS users: `brew install bash`; system bash 3.2 is rejected at runtime)
- **PHP** — host PHP only required for the `host-smoke` backend, lint, build, and audit verbs. PHPUnit itself runs through WP Codebox.
- **Node.js 18.12+** (WP Codebox tooling and ESLint)
- **Composer** (PHP toolchain install)

PHP toolchain pins (from `composer.json`): PHPUnit `^9.0`,
yoast/phpunit-polyfills `^3.0`, squizlabs/php_codesniffer `^3.10`,
wp-coding-standards/wpcs `^3.1`, phpstan/phpstan `^2.0`,
szepeviktor/phpstan-wordpress `^2.0`, wp-phpunit/wp-phpunit `^6.8`.

## Migration from local infrastructure

If a component carries its own bootstrap or PHPUnit config, the runner
warns and ignores it:

```text
⚠ Warning: Local bootstrap.php found and will be IGNORED
  Location: /path/to/plugin/tests/bootstrap.php
  Homeboy WordPress extension provides complete test infrastructure.
```

Files safe to remove after migration:

- `tests/bootstrap.php`
- `phpunit.xml` / `phpunit.xml.dist`
- Local `phpcs.xml`, `phpstan.neon`, `.eslintrc*` (the extension owns these)

The one exception is `phpstan-baseline.neon` — components may keep one
at the root to grandfather pre-existing findings (see PHPStan above).

## Further reading

- [`docs/TESTING.md`](docs/TESTING.md) — canonical test/lint/bench reference, debug markers, sanctioned suppressions, browser-target schema, known gaps
- [`docs/AGENT_CI_WP_CODEBOX.md`](docs/AGENT_CI_WP_CODEBOX.md) — running Data Machine agents on the WP Codebox WordPress execution substrate
- [`docs/PLAYGROUND_DROPIN.md`](docs/PLAYGROUND_DROPIN.md) — `db.php` coexistence with Playground SQLite
- [`docs/CHANGELOG.md`](docs/CHANGELOG.md) — release history
