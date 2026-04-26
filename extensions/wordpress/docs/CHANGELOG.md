# Changelog

All notable changes to the **wordpress** extension will be documented in this file.

## [2.19.0] - 2026-04-26

### Added
- feat(wordpress-bench): surface bootstrap stage timings as __bootstrap scenario

## [2.18.1] - 2026-04-26

### Fixed
- repair lint runner tool resolution

## [2.18.0] - 2026-04-25

### Added
- bench_env passthrough for host-shell vars into Playground

## [2.17.1] - 2026-04-25

### Fixed
- plugin slug honors HOMEBOY_COMPONENT_ID instead of basename

## [2.17.0] - 2026-04-25

### Added
- wp_config_defines setting for per-component wp-config additions (#247)

## [2.16.1] - 2026-04-25

### Fixed
- use BSD-compatible mktemp templates

## [2.16.0] - 2026-04-25

### Added
- bench shared-state + concurrency contract

## [2.15.0] - 2026-04-25

### Added
- multi-line comment style sniff (closes #239)

## [2.14.0] - 2026-04-25

### Added
- add bench capability dispatcher (Phase 4 of #214)

## [2.13.1] - 2026-04-25

### Changed
- extract Playground bootstrap helpers into shared lib

## [2.13.0] - 2026-04-24

### Added
- PHPStan level 7 + include tests + fix baseline integration

## [2.12.0] - 2026-04-24

### Added
- surface auto-fixable lint findings as prominent CTA

## [2.11.0] - 2026-04-23

### Added
- retire host-PHP test backend — Playground only (Phase 3 of #214)

## [2.10.18] - 2026-04-23

### Changed
- replace wp plugin/theme install with pure unzip + atomic rename

## [2.10.17] - 2026-04-23

### Changed
- playground db.php drop-in coexistence — fixture + smoke test

## [2.10.16] - 2026-04-23

### Changed
- playground test discovery — recursive + phpunit.xml.dist-aware

## [2.10.15] - 2026-04-23

### Changed
- playground diagnostics — structured stage logging + classified failures

## [2.10.14] - 2026-04-23

### Changed
- add Playground as opt-in test backend (Phase 1 of #214)

## [2.10.13] - 2026-04-20

### Fixed
- extend hook_callbacks coverage to all WP registration patterns (homeboy#1149)

## [2.10.12] - 2026-04-19

### Fixed
- correct database default docs (#206) + bypass PHPStan parallel worker crash (#207)

## [2.10.11] - 2026-04-18

### Fixed
- make HOMEBOY_FIX_ONLY the single auto-fix contract for all runners (#1145)

## [2.10.10] - 2026-04-18

### Fixed
- allow leading whitespace on top-level namespace/class/use (#1134)

## [2.10.9] - 2026-04-18

### Fixed
- macOS portability — replace grep -P with sed, add bash 4+ gate to test-runner (#1144, #1146)

## [2.10.8] - 2026-04-09

### Changed
- Add FS_CHMOD and FS_METHOD constants to test wp-tests-config

## [2.10.7] - 2026-04-07

### Fixed
- load plugins at muplugins_loaded instead of plugins_loaded in test bootstrap

## [2.10.6] - 2026-04-04

### Fixed
- PSR-4 aware trait placement + body comparison guard

## [2.10.5] - 2026-04-02

### Fixed
- enable post-deploy permission fix for WordPress plugins and themes

## [2.10.4] - 2026-03-23

### Fixed
- atomic swap deploy with backup-before-replace for plugins and themes

## [2.10.3] - 2026-03-23

### Fixed
- strengthen autofixer semantic safety and framework awareness

## [2.10.2] - 2026-03-22

### Fixed
- use .homeboy-build/ staging dir to avoid build/ collision with npm output (fixes #169)
- auto-detect PHP version from Requires PHP header for PHPStan (fixes #178)

## [2.10.1] - 2026-03-21

### Fixed
- rename extracted zip folder to match remote_path basename (fixes #176)

## [2.10.0] - 2026-03-21

### Added
- independent component versioning and continuous release for each extension
- add field_assertion_template for Rust and PHP
- add field_pattern for struct/class property extraction (#818)
- add type_constructors and assertion_templates for Rust and PHP (#818)
- add php -l syntax validation gate for post-write safety
- write lint findings sidecar for categorized issues
- add [contract] grammar for PHP test generation
- add call_sites to fingerprint output (#824)
- audit reference dependencies — resolve WP core + plugin deps
- add git-clone fallback for CI dependency resolution
- auto-discover dependencies from Requires Plugins header
- emit fix plan sidecars for lint and test
- handle HOMEBOY_CHANGED_TEST_FILES for scoped test runs
- write test infrastructure fix results to HOMEBOY_FIX_RESULTS_FILE sidecar
- write structured fix results to HOMEBOY_FIX_RESULTS_FILE sidecar
- emit type_names in fingerprint output
- add crossref extension script for test/production hook analysis
- WP alternatives + WP Filesystem fixers
- add 4 PHPCS auto-fixers for silenced errors, empty catches, readdir loops, and commented code
- add WordPress-aware unused parameter fixer
- add 4 new auto-fixers for PHPCS violations
- support generic npm build scripts
- language grammar files for structural regex engine
- test output parsers for baseline ratchet and failure analysis
- write test results JSON for homeboy test baseline
- add code coverage collection to test runners
- annotation sidecar JSON for CI inline review
- add test_mapping config to Rust and WordPress extensions
- add extends, visibility, properties, hooks to fingerprint
- auto-detect common namespace prefix and tab-aware indentation
- add refactor script with parse_items + extract_shared
- add method_hashes and structural_hashes to PHP fingerprint
- Add fingerprint scripts for Rust and WordPress extensions
- add post:deploy hooks for plugin activation and cache flush
- add since_tag config for @since placeholder replacement
- support HOMEBOY_STEP/HOMEBOY_SKIP for step filtering
- add cleanup_paths to wordpress, nodejs, and rust modules
- granular audit_feature_patterns for WordPress module
- add audit_feature_patterns for undocumented feature detection
- detect and warn about conflicting local test infrastructure
- add PHPStan static analysis integration
- add lint/test config and reorganize scripts
- add autoload error detection before PHPUnit
- add ESLint runner, Swift module, and documentation
- auto-detect text domain from plugin header
- add polyfills dependency, local file detection, build overrides
- add PHPUnit test infrastructure with SQLite support
- add deploy verification, version patterns, and build config

### Changed
- extract bridge framework, delete stale runtime helpers
- wordpress v2.9.0
- add parallel processing to PHPCS/PHPCBF in lint runner
- extract shared helpers for context resolution and component detection
- v2.9.1
- v2.7.0
- share runner step filtering
- split PHPUnit failure parsing into modular parsers
- v2.6.0
- resolve conflicts with main (PR #82 results parsers)
- Add dead code fields to Rust and WordPress fingerprint scripts
- v2.2.1
- Add feature_context rules and richer templates to Rust and WordPress extensions
- Add audit doc config (feature_labels, doc_targets) to WordPress and Rust extensions
- rename modules to extensions across repo
- v2.1.0
- migrate all module manifests to grouped capability structure
- Refactor script organization and update WordPress module configuration
- rebuilt wordpress testing structure for theme support... modularized for scalability and maintainability
- pre-release changes
- finally fixed ongoing blank return 126 code from overly large output
- updated claude rule
- agent tools module added
- Extract shared fixer helper with directory exclusions
- added more automated lint fixers
- Add PHPCS/PHPCBF vendor packages and test infrastructure
- Add in_array strict and short ternary fixers, enhance Yoda fixer
- Add Yoda condition fixer and enhanced lint runner
- added universal build script for wordpress projects.
- added deploy override to wordpress module for reliability during deployment time
- convert module configs to snake_case keys
- Initial commit

### Fixed
- run plugin entry-file load in subprocess to survive fatals
- include PHPStan findings in lint baseline ratchet
- make frontend build non-fatal for PHP-primary plugins
- seed changelogs with current versions for homeboy release
- add skip_test_patterns — exclude JS/JSX/CSS and admin UI from PHP test mapping
- add hook callbacks to internal_calls in fingerprinter
- deploy safety and PHP version preflight
- improve lint safety — PHPStan-safe noops, param rename guardrails, critical-only checks, text domain fixer
- load local WordPress validation dependencies
- add testdox fallback parser for crashed PHPUnit runs
- smart PHPUnit exit code handling when all tests pass
- run reserved-param-fixer on full plugin path for cross-file safety
- reduce audit false positives in PHP fingerprint extraction
- update PHP 8 named argument call sites when renaming reserved params
- auto-detect MySQL credentials from wp-config.php
- skip comment lines in WP filesystem fixer
- prevent WP filesystem fixer from re-replacing already-fixed calls
- empty-catch fixer uses unset() + auto-detect PHP version from composer.json
- expand yoda and short-ternary fixers to cover real-world patterns
- reinstall npm deps when expected local bin is missing
- fix(wordpress-tests): align mysql default database with CI service
- fix(wordpress-tests): default MySQL host to TCP for CI reliability
- avoid replaying full PHPUnit output on failure
- database_type default should be 'auto' not 'sqlite'
- add REGEXP user function for MySQL compatibility in SQLite
- add query filter + SQL_CALC_FOUND_ROWS emulation to SQLite driver
- SQLite driver — depth-counting CREATE TABLE parser + ON DUPLICATE KEY translation
- SQLite driver — strip ON UPDATE CURRENT_TIMESTAMP, implement DESCRIBE/SHOW TABLES/SHOW INDEX
- SQLite test backend + bundle WP-CLI for test environment
- downgrade doctrine/instantiator for PHP 8.2 compat
- replace --threads CLI flag with neon config for PHPStan 2.x
- remove auto-activate from post:deploy hook
- SQLite DDL translation, db.php cleanup, and zero-test detection
- prevent wp_not_installed() from killing PHPUnit process
- include css, ts, js, json in WordPress file_extensions
- rename HOMEBOY_MODULE_PATH env var to HOMEBOY_EXTENSION_PATH in build scripts
- use HOMEBOY_COMPONENT_ID for build artifact naming (#227)
- Fix PHPUnit test discovery — drop broken XML config
- auto-detect MySQL for test runner, fix SQLite driver
- stream PHPUnit output instead of capturing into variable
- handle permission-denied when cleaning stale build artifacts
- restore PHPUnit test output visibility
- auto-detect CPU count and retry single-threaded on PHPStan parallel worker failure
- resolve minimatch ReDoS vulnerability via npm override
- check for pdo_sqlite extension before running SQLite-backed tests
- remove unconditional bootstrapFiles from phpstan.neon.dist (fixes #27)
- bump PHPStan memory limit from 1G to 2G (fixes #25)
- fix ESLint path, PHPUnit output buffering, and MySQL config params
- skip tests by default during builds
- download WordPress on demand instead of bundling
- update test-runner.sh path in build script
- remove /src from default excludes and add PSR-4 validation
- add missing WordPress function stubs to validate-autoload.php
- add failure tracking and summary to build output
- add missing WordPress stubs for autoload validation
- standardize lint behavior and add theme support to autoload validation
- run eslint from plugin directory for correct import resolution
- add WordPress function stubs to autoload validation
- lint improvements and bash 4+ compatibility check
- fixing automated builds
- fixed wordpress build script to handle nested blocks, also updated modules providing cli tools to support direct execution
- use {} instead of {{}} in verify_command for find -exec

## [2.9.0] - 2026-03-21

### Added
- Initial independent component release
