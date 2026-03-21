# Changelog

All notable changes to the **github** extension will be documented in this file.

## [1.1.0] - 2026-03-21

### Added
- independent component versioning and continuous release for each extension
- add field_assertion_template for Rust and PHP
- add field_pattern for struct/class property extraction (#818)
- add type_constructors and assertion_templates for Rust and PHP (#818)
- add php -l syntax validation gate for post-write safety
- write lint findings sidecar for categorized issues
- add [contract] grammar for PHP test generation
- add [contract.type_defaults] for test input construction
- add call_sites to fingerprint output (#824)
- add call_sites to fingerprint output
- add [contract.test_templates] for test source generation
- add [contract] grammar section for function body analysis
- add cargo fix step to lint runner + format script
- audit reference dependencies — resolve WP core + plugin deps

### Changed
- extract bridge framework, delete stale runtime helpers
- wordpress v2.9.0
- rust v1.9.0
- add parallel processing to PHPCS/PHPCBF in lint runner
- rust extension v1.8.0 — decompose import resolution fixes

### Fixed
- seed changelogs with current versions for homeboy release
- add skip_test_patterns — exclude JS/JSX/CSS and admin UI from PHP test mapping
- add hook callbacks to internal_calls in fingerprinter
- comprehensive decompose import resolution — trait imports, super:: paths, doc comments
- decompose import resolver now detects functions, constants, and glob imports

## [1.0.0] - 2026-03-21

### Added
- Initial independent component release
