# Changelog

## [2.8.0] - 2026-03-10

### Added
- Add Kimaki extension with doctor, model inspection, and fallback repro commands
- Support focused Kimaki diagnostics with --thread, --session, and --recent filters
- add kimaki diagnostics extension
- emit fix plan sidecars for lint and test
- handle HOMEBOY_CHANGED_TEST_FILES for scoped test runs
- write test infrastructure fix results to HOMEBOY_FIX_RESULTS_FILE sidecar
- write structured fix results to HOMEBOY_FIX_RESULTS_FILE sidecar
- emit type_names in fingerprint output
- add crossref extension script for test/production hook analysis

### Fixed
- keep kimaki extension changes unreleased
- add testdox fallback parser for crashed PHPUnit runs
- scope cargo fmt --check to changed files in CI
- recognize Rust shorthand field init syntax
- smart PHPUnit exit code handling when all tests pass
- run reserved-param-fixer on full plugin path for cross-file safety
- normalize test method names in fingerprint and add skip_test_patterns
- reduce audit false positives in PHP fingerprint extraction
- update PHP 8 named argument call sites when renaming reserved params
- auto-detect MySQL credentials from wp-config.php

## [2.6.0] - 2026-03-04

### Added
- language grammar files for structural regex engine
- test output parsers for baseline ratchet and failure analysis
- write test results JSON for homeboy test baseline
- add code coverage collection to test runners (#75)
- annotation sidecar JSON for CI inline review (#74)
- add test_mapping config to Rust and WordPress extensions (#73)

### Changed
- Add CHANGELOG.md
- resolve conflicts with main (PR #82 results parsers)

### Fixed
- database_type default should be 'auto' not 'sqlite' (#84)
- add REGEXP user function for MySQL compatibility in SQLite
- add query filter + SQL_CALC_FOUND_ROWS emulation to SQLite driver
- SQLite driver — depth-counting CREATE TABLE parser + ON DUPLICATE KEY translation (#79)
- SQLite driver — strip ON UPDATE CURRENT_TIMESTAMP, implement DESCRIBE/SHOW TABLES/SHOW INDEX
- SQLite test backend + bundle WP-CLI for test environment
- downgrade doctrine/instantiator for PHP 8.2 compat

## [2.7.0] - 2026-03-07

### Added
- WP alternatives + WP Filesystem fixers (#108)
- add 4 PHPCS auto-fixers for silenced errors, empty catches, readdir loops, and commented code (#103)
- add WordPress-aware unused parameter fixer (#102)
- add 4 new auto-fixers for PHPCS violations (#101)
- support generic npm build scripts (#99)

### Changed
- split monolithic refactor.py into package (#109)

- feat: language grammar files for structural regex engine
- feat: test output parsers for baseline ratchet and failure analysis
- feat: write test results JSON for homeboy test baseline
- feat: add code coverage collection to test runners
- feat: annotation sidecar JSON for CI inline review
- feat: add test_mapping config to Rust and WordPress extensions
- fix: database_type default should be 'auto' not 'sqlite'
- fix: add REGEXP user function for MySQL compatibility in SQLite
- fix: add query filter + SQL_CALC_FOUND_ROWS emulation to SQLite driver
- fix: SQLite driver — depth-counting CREATE TABLE parser + ON DUPLICATE KEY translation
- fix: SQLite driver — strip ON UPDATE CURRENT_TIMESTAMP, implement DESCRIBE/SHOW TABLES/SHOW INDEX
- fix: SQLite test backend + bundle WP-CLI for test environment
- fix: downgrade doctrine/instantiator for PHP 8.2 compat
- share runner step filtering (#98)
- split PHPUnit failure parsing into modular parsers

### Fixed
- skip comment lines in WP filesystem fixer
- prevent WP filesystem fixer from re-replacing already-fixed calls
- improve bodyless trait method detection for unused parameter analysis (#107)
- skip unused parameter detection for trait method declarations (#106)
- eliminate unused_parameter false positives from type path segments (#105)
- empty-catch fixer uses unset() + auto-detect PHP version from composer.json (#104)
- expand yoda and short-ternary fixers to cover real-world patterns (#100)
- narrow cfg(test) module detection
- handle rust lifetimes in boundary parsing
- avoid duplicate failure output replay in rust runner (#93)
- reinstall npm deps when expected local bin is missing (#92)
- fix(wordpress-tests): align mysql default database with CI service (#89)
- fix(wordpress-tests): default MySQL host to TCP for CI reliability (#88)
- avoid replaying full PHPUnit output on failure

## [2.5.0]
- WordPress extension with WP-CLI integration
