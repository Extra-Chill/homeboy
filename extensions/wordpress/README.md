# Homeboy WordPress Extension

Homeboy extension that gives WordPress plugins and themes a complete
`test → lint → build → bench → trace → audit` pipeline with zero in-component
configuration. PHPUnit and benchmark workloads run through [WP Codebox][wp-codebox]
by default, so there is no host PHP, MySQL, or local WordPress install to manage.

[wp-codebox]: https://github.com/Automattic/wp-codebox
[playground]: https://playground.wordpress.net/

## What this extension provides

It registers the `wordpress` component kind with Homeboy core and wires up
extension scripts for these verbs:

| Homeboy verb | What it does | Entry script |
|---|---|---|
| `test` | PHPUnit and real-WordPress host smokes via WP Codebox | `scripts/test/test-runner.sh` |
| `lint` | PHPCS + PHPStan (PHP) and ESLint (JS/TS) | `scripts/lint/lint-runner.sh` |
| `build` | Production ZIP with composer `--no-dev`, asset build, syntax check | `scripts/build/build.sh` |
| `bench` | Benchmark workloads via the WordPress bench runtime backend; optional browser handoff | `scripts/bench/bench-runner.sh` |
| `trace` | Project-owned scenario traces | `scripts/trace/trace-runner.sh` |
| `audit` | Detector rules over PHP for lifecycle / role tagging | `scripts/audit/setup-references.sh` + rules in `wordpress.json` |
| `fingerprint` | File-shape fingerprinting for change detection | `scripts/fingerprint.sh` |
| `refactor` | Auto-fix pass (PHPCBF + custom fixers) | `scripts/refactor.py` |
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

## Bench Helpers

`scripts/bench/bench-runner.sh` selects the WordPress bench runtime backend.
The default backend is `wp-codebox`, preserving the existing WP Codebox
sandbox/artifact behavior. Set `HOMEBOY_WORDPRESS_BENCH_RUNTIME_BACKEND` to
select a backend; currently supported value: `wp-codebox`.

Reusable WordPress/WooCommerce workload helpers live under `scripts/bench/lib/`.
Workloads can require them from the WP Codebox-mounted extension path.

### REST Route Discovery

`lib/wordpress-rest-route-discovery.js` provides generic WordPress REST route
discovery helpers for fuzz orchestration. Callers can pass a captured REST index
plus optional per-route `OPTIONS` and schema snapshots, or provide a `fetch`
implementation and `baseUrl` to collect the same metadata through the REST API.
The normalized artifact uses `homeboy/wordpress-rest-route-discovery/v1` and
records route, method, namespace, argument summary, response schema summary, and
best-effort auth metadata without embedding product-specific endpoints.

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

### Real-WordPress host smokes

Standalone smoke files matching `tests/**/*-smoke.php` are diagnostic/operator
targets, not default release gates. A component can declare a default and exact
per-file environment overrides in a root `homeboy-test-manifest.json`:

```json
{
  "schema": "homeboy/test-manifest/v1",
  "default_environment": "standalone-php",
  "tests": {
    "tests/runtime-smoke.php": { "environment": "wordpress" }
  }
}
```

`standalone-php` files run directly with PHP, while `wordpress` files are
mounted with the component and executed via `wordpress.run-php`. An exact
`tests` entry overrides `default_environment`. Omitting `default_environment`
keeps the existing `wordpress` default.

To rerun one existing smoke on demand before pushing:

```bash
homeboy test <component-id> -- --host-smoke-file tests/example-smoke.php
```

Use `--file` for a manifest-declared standalone PHP smoke. The focused command
does not change default test discovery or add smokes to CI.
Output preserves the machine-readable `HOST_SMOKE_BEGIN`,
`HOST_SMOKE_PROGRESS`, `HOST_SMOKE_OK`, `HOST_SMOKE_FAIL`, and
`HOST_SMOKE_SUMMARY` markers.

### Test runtime backend

`scripts/test/test-runner.sh` selects a generic real-WordPress runtime backend
for PHPUnit and core-dev test runs. The current backend implementation is
`wp-codebox`, and it remains the default:

```bash
HOMEBOY_WORDPRESS_TEST_RUNTIME_BACKEND=wp-codebox homeboy test <component-id>
```

The `test-runner-wp-codebox.sh` script name and existing WP Codebox settings are
preserved for compatibility with the current implementation.

### Fatal runtime crashes

A PHP-WASM trap (`RuntimeError: null function or function signature mismatch`
and friends) leaves the interpreter unusable, but the WP Codebox CLI logs the
rejection instead of exiting — so the recipe-run promise never settles and the
run sits until its budget expires. On #12617 that turned a crash which happened
in the first seconds into a 24-minute shard, reported only as a timeout.

The adapter watches recipe-run output for an *unclaimed* fatal error — an
unhandled rejection or uncaught exception carrying a WebAssembly `RuntimeError`
— and arms a short deadline instead of waiting out the budget. A stack frame
mentioning `php.wasm` on its own does not qualify; traces get logged for
non-fatal reasons.

The grace window is what makes acting on the signature safe: a run that somehow
recovers and finishes inside it is untouched, while a wedged one costs about a
minute instead of the full budget.

```bash
# default 60s; 0 disables early termination
HOMEBOY_WP_CODEBOX_RUNTIME_CRASH_GRACE_SECONDS=60 homeboy test <component-id>
```

The same value can be set as `wp_codebox_runtime_crash_grace_seconds` in
extension settings. When a crash is detected the run reports
`PHPUNIT_ZERO_TESTS cause=runtime_crashed`, and
`files/wp-codebox-timeout-diagnostics.json` carries
`termination.result = "runtime_crash"` plus a `runtime_crash` object naming the
signature — so the failure names the crash rather than the elapsed budget.

### Runtime dependencies

If a plugin depends on other local plugins at runtime, declare them via
`validation_dependencies`. They are mounted alongside the plugin under
test and loaded during the `load_deps` bootstrap stage:

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

#### Shared ruleset for consumers

[`rulesets/homeboy-wordpress-project.xml`](rulesets/homeboy-wordpress-project.xml)
is the reusable half of `phpcs.xml.dist`, shipped so any WordPress
component can reference it instead of hand-rolling PHPCS config. It
includes `WordPress-Extra` with the PSR-4 naming exclusions, the
PEAR/Squiz spacing exclusions (they fight WordPress spacing rules and
cause PHPCBF infinite loops), `testVersion`, and exclude-patterns for
`vendor/` (plus its prefixed/scoped variants), `node_modules/`,
`build/`, `dist/`, `tools/`, and `scoper.inc.php`. `phpcs.xml.dist`
above references this same file (`<rule ref="rulesets/homeboy-wordpress-project.xml"/>`),
so the extension's own lint run is the proof that the shipped artifact
works — it is not a parallel, drifting copy.

A component with no PHPCS config of its own (no `phpcs.xml`, `.phpcs.xml`,
`phpcs.xml.dist`, or `.phpcs.xml.dist`) gets this shared ruleset automatically
from the Lint capability — `homeboy lint`, `homeboy review`, and the
`preflight.lint` release gate all resolve to it, with no `scripts.lint` or
composer wiring required. The fallback is this file specifically, not
`phpcs.xml.dist` above: `phpcs.xml.dist` references the shared ruleset via a
relative `<rule ref>` that only resolves when PHPCS runs from this extension's
own directory, which is true for this extension's self-check lint but never
true for a consumer component (homeboy-extensions#2797).

**Running it standalone** — a WordPress extension install exposes its
own path via `homeboy extension exec`, which sets `HOMEBOY_EXTENSION_PATH`
in the child process environment:

```bash
homeboy extension exec wordpress -c <component-id> -- \
    vendor/bin/phpcs --standard="$HOMEBOY_EXTENSION_PATH/rulesets/homeboy-wordpress-project.xml" .
```

Drop `-c <component-id>` to run against the current working directory
instead of a Homeboy-registered component. `vendor/bin/phpcs` there is
the *consumer's own* Composer-installed PHPCS binary (`squizlabs/php_codesniffer`
+ `wp-coding-standards/wpcs` as `require-dev`) resolved relative to the
component's working directory — the extension only supplies the
ruleset file, not the PHPCS binary itself, unless the consumer has none
installed and falls back to the extension's own `vendor/bin/phpcs`.

> **Note on `{{extensionPath}}`:** this repository's own `composer.json`
> (`"test": "phpunit --configuration={{extensionPath}}/phpunit.xml.dist"`)
> looks like a working example of a `{{extensionPath}}` substitution
> that Composer scripts can use directly. It is not — `{{extensionPath}}`
> is only substituted inside `runtime.run_command` and action `command`
> templates that Homeboy itself invokes (`homeboy extension run` /
> `extension.run` pipeline steps); Composer never sees or expands it.
> Running `composer test` in this repo fails literally
> (`Could not read "{{extensionPath}}/phpunit.xml.dist"`) — confirmed
> while building this ruleset. The one-liner above using
> `homeboy extension exec` + the real `HOMEBOY_EXTENSION_PATH`
> environment variable is the actual, currently-working resolution
> path. A follow-up issue tracks fixing or removing the misleading
> `composer.json` entry; it is unrelated to the PHPCS ruleset and out
> of scope here.

**Consumer overrides** — reference the whole file with a single `<rule
ref>` and layer overrides in your own `phpcs.xml.dist` without copying
the exclusion list. PHPCS's `<rule ref>` does not expand shell
variables or `~`, so a static ruleset file needs the extension's
resolved absolute path — get it once with
`homeboy extension exec wordpress -- pwd` (or `homeboy extension show wordpress`)
and hardcode it, same as any other vendored tool path in a project
config:

```xml
<?xml version="1.0"?>
<ruleset name="My Component">
    <rule ref="/home/you/.config/homeboy/extensions/wordpress/rulesets/homeboy-wordpress-project.xml"/>

    <!-- Add an exclusion the shared file doesn't have -->
    <exclude-pattern>*/legacy/*</exclude-pattern>

    <!-- Re-enable a specific message the shared ruleset excludes.
         A bare <rule ref="Code"/> does NOT undo an inherited exclusion —
         PHPCS only resets severity when you set it explicitly. -->
    <rule ref="WordPress.NamingConventions.ValidVariableName.VariableNotSnakeCase">
        <severity>5</severity>
    </rule>
</ruleset>
```

This is a real limitation, not a templating gap this PR could paper
over: it is inherent to how PHPCS parses `<rule ref>` paths (verified
directly — `~/...` in a `<rule ref>` fails with "Referenced sniff ...
does not exist"). Consumers who never need to override anything are
better served by the CLI one-liner above, which resolves
`$HOMEBOY_EXTENSION_PATH` in the shell before PHPCS ever sees the path.

Adding an `<exclude-pattern>` is always safe to layer on top. Removing
one is not possible once inherited through `<rule ref="...">` — PHPCS
exclude-patterns are additive only. That is why `*/tests/*` is **not**
part of the shared default: a consumer who wants tests out of scope can
add the pattern above in one line, but a consumer who wants tests
linted could never undo it if the shared file excluded them. Consumers
whose `tests/` trees are not WordPress-Extra-clean (PHPUnit fixtures,
smoke harnesses, etc., as in this extension's own `tests/`) add the
exclusion locally — see how `phpcs.xml.dist` in this repo does exactly
that as a host-specific addition on top of the shared file.

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
    ├─ Build frontend assets     (@wordpress/scripts when present)
    ├─ Install production deps   (composer --no-dev)
    ├─ Stage into .homeboy-build/  (avoids @wordpress/scripts build/ collision)
    ├─ Copy files                (rsync, or an exact package profile)
    ├─ Validate build structure  (php -l, PSR-4)
    ├─ ZIP → build/<component-id>.zip
    └─ Restore dev dependencies
```

Pre-build validation runs `scripts/build/validate-build.sh`. PHP syntax
errors and PSR-4 violations block the build; lint findings do not.

Production builds exclude arbitrary nested `*.zip` files by default to avoid
shipping stale release artifacts. Components that intentionally need ZIP package
artifacts inside the deployed plugin or theme can declare explicit include globs:

```json
{
	"extensions": {
		"wordpress": {
			"package_artifacts": [
				"runtime/**/packages/*.zip"
			],
			"package_excludes": [
				"/playground-runtime/",
				"runtime/**/source-maps/"
			]
		}
	}
}
```

`package_artifacts` patterns are component-relative, support recursive `**`
matching, must not contain absolute or traversal paths, and must match at least
one regular file. Matching files are sorted, deduplicated, copied into staging
after the default rsync excludes, and reported with SHA-256 values in the build
output. The managed `.homeboy-build/` staging directory is never eligible.

`package_excludes` adds rsync exclusions without replacing `.buildignore`, the
defaults, or mandatory Homeboy safety exclusions. Use component-root rsync
patterns such as `/playground-runtime/` to exclude a root directory. Exclude
patterns are strings and cannot traverse outside the component.

Components that already declare an authoritative runtime package profile can
point the build at that contract instead of duplicating its selectors as rsync
exclusions:

```json
{
	"extensions": {
		"wordpress": {
			"package_profile": {
				"manifest": "package-manifest.json",
				"profile": "runtime"
			}
		}
	}
}
```

`package_profile.manifest` is a component-relative JSON file. `package_profile.profile`
names an entry under `profiles`. Each profile provides `selectors` with
`type` `file` or `prefix` and a component-relative `path`, plus optional
`required_files`. Selection runs after production dependency preparation so
generated `vendor/` files are eligible. When the setting is present, staging
copies exactly the selected regular files under the existing package root.
Homeboy still excludes `.homeboy-build/`. Invalid manifest or profile names,
absolute paths, traversal, malformed selectors, unmatched explicit files, and
missing required files fail closed. Components without this setting keep the
current rsync behavior. The build writes a deterministic selected-file
inventory plus the manifest and profile identity as
`wordpress.package_profile` evidence.

### Additional release package profiles

A component that must preserve its full default package while also publishing
narrower profile packages can declare additional release package profiles. The
primary `package_profile` ZIP is built unchanged; each additional entry is
built from the same release source and production dependencies and published as
its own release asset:

```json
{
	"extensions": {
		"wordpress": {
			"settings": {
				"package_profile": {
					"manifest": "package-manifest.json",
					"profile": "full"
				},
				"additional_package_profiles": [
					{
						"manifest": "package-manifest.json",
						"profile": "runtime",
						"artifact": "my-plugin-runtime.zip"
					}
				]
			}
		}
	}
}
```

Each entry pairs an existing `{manifest, profile}` declaration with a release
asset `artifact` name. Manifest and profile paths reuse the same fail-closed
`package_profile` validation as the primary profile. Artifact names must be
basename-only `.zip` names, unique across entries, and cannot replace the
primary `build/<component-id>.zip` asset; path-like, traversal, non-ZIP, and
duplicate names are rejected before any build runs, and malformed declarations
fail closed.

`release.package` builds the primary ZIP first, then rebuilds through the same
production `build.sh` path once per entry with `package_profile` overridden to
that entry. Every emitted ZIP is verified to contain the release version before
it can reach the publish step. The artifact JSON lists the primary ZIP first,
followed by each additional asset in declared order; components without this
setting keep emitting exactly their existing primary ZIP.

### Release provenance preparation

Components that need release-specific preparation or provenance can set
`release_provenance_command` in their WordPress extension settings. The command
runs after `release.package` stages the ZIP. It writes a JSON object to the
provided sidecar path; a nonzero exit or missing/invalid sidecar aborts release.

```json
{
  "extensions": {
    "wordpress": {
      "settings": {
        "release_provenance_command": "my-provenance-tool > \"$HOMEBOY_WORDPRESS_RELEASE_SIDECAR_PATH\""
      }
    }
  }
}
```

The command receives `HOMEBOY_WORDPRESS_RELEASE_PACKAGE_ROOT`, source
`VERSION`, `TAG`, and `COMMIT`, plus `ZIP_PATH`, `OUTPUT_PATH`, and
`SIDECAR_PATH` (all prefixed with `HOMEBOY_WORDPRESS_RELEASE_`). The package
root and output paths are resolved inside the component checkout. The extension
seals the sidecar with schema version `1`, the release source coordinates, and
the ZIP SHA-256. `release.publish` requires those exact values from its release
payload, uploads the sidecar before the ZIP, verifies both remote asset names,
rolls back either named asset after a failed transaction, and emits ZIP and
sidecar digests in its receipt.

### Local workspace dependency overrides

A component can depend on a sibling workspace package that is intentionally not
published — "cooked locally" on a branch (e.g. an `@scope/ui` package in a
sibling pnpm monorepo built on `trunk`). An npm `file:` install symlinks the
dependency's own `node_modules`, so its peer deps (React) resolve to a *second*
copy → "Invalid hook call" → a blank app. Declare the dependency instead and the
build will build it from source, pack it, and install the built tarball so peer
deps dedupe to the consumer's single copy:

```json
{
	"extensions": {
		"wordpress": {
			"local_workspace_dependencies": [
				{
					"name": "@automattic/agenttic-ui",
					"path": "../agenttic",
					"package_dir": "packages/agenttic-ui",
					"build": "pnpm install --frozen-lockfile && pnpm --filter @automattic/agenttic-ui build",
					"package_manager": "pnpm"
				}
			]
		}
	}
}
```

Only `name` and `path` are required. `path` is resolved relative to the
component (sibling repos via `..` are allowed) and `name` is validated against
the resolved package. When `build` is omitted the dependency's own `build`
script is run after installing its dependencies. The override runs after the
consumer's own dependencies are installed and before the consumer build, and a
declared override that fails is a fatal build error. The mechanism is generic
Node.js behavior shared with the `nodejs` build runner
(`nodejs/scripts/lib/local-workspace-deps.sh`); it carries no WordPress
specifics.

## Bench runner

Bench workloads run through WP Codebox. WP Codebox owns the disposable WordPress
runtime, component mount, command execution, and artifacts, and emits the same
`BenchResults` envelope Homeboy core parses.

Each file under `tests/bench/*.php` returns a callable. The callable may return
numeric metrics directly or `{metrics, metadata, artifacts}`. Components may also
declare configured workloads via the canonical `wordpress_runtime_workloads`
setting; the WordPress runner maps those declarations into a temporary WP
Codebox recipe alongside `validation_dependencies`, `wp_codebox_file_mounts`,
`wp_config_defines`, `bench_env`, shared-state mounts, and browser handoff
descriptors.

The generated recipe is the single runtime entry point for benchmarks:
`wordpress_runtime_blueprint` becomes `runtime.blueprint`, dependencies become recipe
plugin inputs, and scenario manifests compile into configured workloads.
Fixture profiles can seed the sandbox through WP Codebox `inputs.siteSeeds`
without product-specific recipe steps:

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

The bridge also accepts `fixtureProfile` and `wp_codebox_fixture_profile` when
building the recipe. Homeboy Extensions only maps and validates the profile
shape; WP Codebox owns seed import behavior and runtime validation.

Each run also emits `${HOMEBOY_BENCH_RESULTS_ARTIFACT_DIR}/bench-summary.json`
when result artifacts are enabled. The summary is the canonical reviewer
entry point: it records pass/fail score, replayability status, dependency
provenance, core artifact paths, and any caller-provided next-step commands.
Dependency inputs are also captured in `bench-dependency-provenance.json` so
reviewers can see which WP Codebox plugin inputs, mounts, and declared
dependency paths shaped the run.

`bench_env` must be a JSON object and is the extension-level setting forwarded
into the WP Codebox runtime. Unknown caller/orchestrator settings remain opaque;
the runner only validates the shape of settings it consumes directly.

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

### Helper manifest consumers

Installed WordPress extensions expose helper paths through
`HOMEBOY_WORDPRESS_HELPER_MANIFEST`. Node/ESM workloads can consume that
manifest without copying `createRequire()` boilerplate:

```js
import helperConsumer from 'homeboy-extension-wordpress/wordpress-helper-consumer';

const { loadWordPressHelper, loadWordPressLibHelper } = helperConsumer;
const { path: profilerPath, module: profiler } = loadWordPressHelper('requestProfiler');
const { module: pageProfiler } = loadWordPressLibHelper('page-profiler.js', { required: true });
```

The loader returns stable handles shaped as `{ path, module, found, reason }`.
Missing optional helpers return `module: null`; pass `{ required: true }` to fail
with a diagnostic error.

Request-profiler JSONL rows can be compacted for benchmark artifacts with
`summarizeWordPressRequestProfilerRows(rows, options)`. The summary groups rows
by `request_id`, extracts final URI/method/duration/status, and includes bounded
`requests`, `slow_requests`, `hooks`, and `timing_rows` arrays. Product-specific
route attribution and gate decisions should stay in the calling rig.

### Block quality probes

`probeWordPressBlockQuality(sitePath, options)` collects site-wide block quality
counters and can include a target post/page aggregate for scenario gates. Target
posts can be selected with `targetPostIds`, `targetPostTitles`, and
`includeFrontPageTarget`; the result includes counters such as
`target_pages_seen`, `target_posts_with_blocks`, `target_total_blocks`, and
`target_core_html_blocks` alongside the whole-site totals. When
`fallbackOptionNames` is set, `target_core_html_without_fallback` and the
Studio-compatible `target_core_html_without_bfb_fallback` subtract that fallback
total from the target `core/html` count.

`probeWordPressPostBlockQuality(sitePath, postId, options)` remains available for
single-post diagnostics where stored-content metadata and previews are useful.

### Materialized site quality gates

`evaluateMaterializedSiteQuality(input, options)` evaluates a materialized
WordPress site from importer, native block, editor parity, frontend parity, and
semantic summary inputs. It returns `passed`, `failureDetails`, stable
`failureReasons`, and flattened benchmark `metrics`. Product rigs own policy and
thresholds; pass options such as `visualPixelDiffThreshold` and
`visualEditorPixelDiffThreshold` instead of hardcoding product gates in this
helper.

Stable metric names include `semantic_mismatch_count`,
`importer_core_html_block_count`, `importer_freeform_block_count`,
`importer_fallback_count`, `importer_invalid_block_count`,
`visual_editor_vs_source_pixel_diff_ratio`,
`visual_editor_vs_frontend_pixel_diff_ratio`,
`visual_source_vs_frontend_pixel_diff_ratio`, `visual_pixel_diff_ratio`,
`native_block_quality_pass`, `native_block_quality_failure_count`,
`success_rate`, `agent_error_rate`, `timed_out`, and `agent_runner_error`.

Stable failure reason codes include `semantic_mismatch`,
`importer_core_html_blocks`, `importer_freeform_blocks`,
`importer_fallback_blocks`, `importer_invalid_blocks`,
`editor_visual_parity_error`, `editor_source_visual_diff`,
`editor_frontend_visual_diff`, `source_frontend_visual_diff`,
`missing_target_block_page`, `agent_authored_wp_html`,
`core_html_without_bfb_fallback`, `bfb_fallback`, `importer_report_error`,
`editor_invalid_blocks`, `editor_validation_error`, `agent_timed_out`, and
`agent_runner_error`.

```js
const {
	installWordPressRequestProfiler,
	collectWordPressRequestProfiles,
	summarizeWordPressRequestProfilerRows,
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

const wordpressRequestSummary = summarizeWordPressRequestProfilerRows(
	collectWordPressRequestProfiles(sitePath),
	{ slowThresholdMs: 50 }
);
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
  --setting provider=provider-slug \
  --setting model=provider/model \
  --setting provider_plugin_paths=/components/ai-provider-example \
  --setting secret_env=PROVIDER_API_KEY
```

Generic audit fanout planning and reconcile mechanics live in
`../runtime-agent-ci`. The WordPress extension does not select or execute an AI
runtime for `homeboy refactor`; provider orchestration belongs to an explicit
agent-task provider rather than the deterministic refactor hook.

### WordPress runtime task planner

`lib/wordpress-runtime-task-planner.js` and
`scripts/agent/homeboy-wordpress-runtime-task-plan.cjs` keep WordPress/Codebox/DLA
orchestration in this extension by projecting runtime-task intent into Homeboy's
generic `homeboy/agent-task-plan/v1` and `homeboy/agent-task-request/v1` contract.
Callers provide the ability, ability input, backend/provider/runtime selection,
fanout/concurrency metadata, expected artifacts, timeout, and optional DLA URL
shorthand; Homeboy core only sees generic durable agent-task plans.

Direct WP Codebox integration lives in this extension: its client resolves,
preflights, and invokes the public runtime CLI while consuming public runtime
contracts and artifact references. It is not a Homeboy agent-task provider.

WordPress fuzz runtime actions use
`homeboy/wordpress-fuzz-runtime-workload-operation/v1` descriptors. Mapping those
actions to WP Codebox requires a public WP Codebox runtime action contract; the
extension emits explicit blockers instead of guessing Codebox commands or ability
names when that contract is absent. See
[`docs/WP_CODEBOX_RUNTIME_ACTION_CONTRACTS.md`](docs/WP_CODEBOX_RUNTIME_ACTION_CONTRACTS.md).

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
| `validation_dependencies` | array | `[]` | Component IDs, paths, or scoped objects to mount during PHPStan, autoload validation, PHPUnit, and bench |
| `user` | string | `""` | WP-CLI user (email/login/ID); appended as `--user` when set |
| `wp_config_defines` | object | `{}` | `CONSTANT_NAME => value` map appended to the runtime `wp-tests-config.php`; PHP type preserved via `var_export` |
| `bench_env` | object | `{}` | `NAME => value` env vars forwarded into the runtime (workloads/fixtures read via `getenv()`) |
| `wp_codebox_core_module` | string | `""` | Host-side ESM module path or package specifier that exports WP Codebox recipe builders for bench recipe generation |
| `wordpress_runtime_blueprint` | object | `{}` | Runtime blueprint merged into the generated recipe |
| `wp_codebox_extra_plugins` | array | `[]` | Additional plugin entries that are not Homeboy validation dependencies |
| `wordpress_runtime_workloads` | array | `[]` | Workloads passed to `wordpress.bench` through the generated recipe after deps and component load |
| `wp_codebox_file_mounts` | array | `[]` | Files from the component or validation dependencies mounted into explicit WordPress runtime paths |
| `fixture_profile` | object | `{}` | Product-agnostic fixture profile mapped to WP Codebox `inputs.siteSeeds` for sandbox setup before fuzz/coverage workloads run |
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
WP Codebox Playground runtime. Set the legacy script-specific
`HOMEBOY_WP_CODEBOX_BIN` override to validate with a specific wp-codebox binary.

## WP Codebox Doctor

Use `scripts/doctor/wp-codebox-doctor.sh` when the WordPress runner looks stuck
or the Playground runtime cache may be bad:

```bash
wordpress/scripts/doctor/wp-codebox-doctor.sh doctor
wordpress/scripts/doctor/wp-codebox-doctor.sh cleanup --stale-after-seconds 3600
```

The script resolves `runtime_bin` first, then legacy `HOMEBOY_WP_CODEBOX_BIN` or
`wp_codebox_bin` from Homeboy settings, before delegating to upstream
`wp-codebox doctor` or `wp-codebox cleanup`.
WP Codebox owns the health output, including JSON mode, binary/source checks,
stale `recipe-run` process checks, and archive cache cleanup behavior.

### Source hydration

Source installs require the checked-out WP Codebox source to provide an npm
lockfile (`package-lock.json` or `npm-shrinkwrap.json`). Homeboy hydrates it with
`npm ci --include=optional`, which removes stale dependencies and installs the
current platform's optional native packages before checking both the CLI and the
`sharp` native runtime.

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
| `HOMEBOY_LINT_FAIL_ON=errors\|warnings` | PHPCS gate severity (default `errors`: warnings reported but non-blocking; `warnings` restores legacy block-on-warning). ESLint already gates on errors only. |

## Blocking vs advisory

| Check | Behavior |
|---|---|
| PHPCS | **Blocks on errors.** Warnings are reported (summary + findings) but non-blocking by default; set `HOMEBOY_LINT_FAIL_ON=warnings` to also block on warnings. |
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
- [`docs/PLAYGROUND_DROPIN.md`](docs/PLAYGROUND_DROPIN.md) — `db.php` coexistence with Playground SQLite
- [`docs/CHANGELOG.md`](docs/CHANGELOG.md) — release history
