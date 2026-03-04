# Changelog

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

## [Next]

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

## [2.5.0]
- WordPress extension with WP-CLI integration
