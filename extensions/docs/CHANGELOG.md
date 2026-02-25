# Changelog

## [2.1.0] - 2026-02-25

### Added
- WordPress post:deploy hooks for plugin activation and cache flush

### Fixed
- WordPress build script permission errors on stale artifacts
- PHPUnit test output swallowed instead of streamed to stdout

## [0.5.0] - 2026-02-13

- feat: add openclaw module for AI agent management

## [0.4.0] - 2026-02-10

### Added
- plasma-shield module with desktop visual UI

### Fixed
- WordPress module: download WordPress on demand instead of bundling
- plasma-shield: rename manifest to {id}.json pattern

## [0.3.0] - 2026-02-10

### Fixed
- WordPress deploy override now injects `--allow-root` for root SSH users

## [0.2.0] - 2026-01-23

### Added
- Auto-init context injection for agent hooks

### Refactored
- GitHub module: Remove CLI wrapper, focus on release publishing

### Fixed
- Add missing WordPress function stubs to validate-autoload.php
- Add ^ anchors to prevent false positive pattern matches

## [0.1.4] - 2026-01-22

- fix: anchor all anti-pattern regexes to command start for consistency

## [0.1.3] - 2026-01-22

- feat: add CLI help configuration to WordPress module

## [0.1.2] - 2026-01-21

### Fixed
- lint-runner.sh now surfaces clear error when Text Domain header is missing instead of dying silently
- PHP fixers now exclude vendor/, node_modules/, and build/ directories via shared fixer-helpers.php
- Fixed silent exit 126 failure when linting plugins with many errors (JSON output exceeding macOS ARG_MAX limit now piped via stdin)

## [0.1.1] - 2026-01-19
- Initial release
