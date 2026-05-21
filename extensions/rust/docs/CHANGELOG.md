# Changelog

All notable changes to the **rust** extension will be documented in this file.

## [1.17.4] - 2026-05-21

### Fixed
- normalize sidecar contracts across extensions

## [1.17.3] - 2026-05-20

### Fixed
- handle nested changed tests

## [1.17.1] - 2026-05-19

### Fixed
- handle multiple inline test scopes

## [1.17.0] - 2026-05-19

### Added
- add compiler warning contracts
- emit aggregate construction fingerprints
- declare structured sidecar capabilities

### Fixed
- stop self-packaging extension releases
- allow dirty crate publishes
- scope inline test modules
- scope changed integration tests
- fail scoped zero-test runs
- verify Rust autofixes compile
- report lint autofix results

## [1.16.1] - 2026-05-08

### Fixed
- sync lockfile before release commit

## [1.16.0] - 2026-05-06

### Added
- feat(rust/audit): declare trivial/plumbing call lists for duplication detector
- declare lockfile_paths for autofix drift resolution
- add post-write validate and format scripts

### Fixed
- fix(rust/audit): nest duplication detector config under detector_rules

## [1.14.0] - 2026-04-30

### Added
- add fingerprint metadata to grammars

## [1.13.1] - 2026-04-29

### Fixed
- avoid grep in test result parser

## [1.13.0] - 2026-04-27

### Added
- declare drift selection config

## [1.12.1] - 2026-04-27

### Fixed
- adopt shared resolve context

## [1.12.0] - 2026-04-27

### Added
- support list-only scenario discovery

## [1.11.0] - 2026-04-26

### Added
- feat(rust-bench): add bench capability to dispatch cargo workloads

## [1.10.0] - 2026-04-25

### Added
- independent component versioning and continuous release for each extension
- add field_assertion_template for Rust and PHP
- add field_pattern for struct/class property extraction (#818)
- add type_constructors and assertion_templates for Rust and PHP (#818)
- add [contract] grammar for PHP test generation
- add [contract.type_defaults] for test input construction
- add [contract.test_templates] for test source generation
- add [contract] grammar section for function body analysis
- add cargo fix step to lint runner + format script
- add generate_module_index command
- handle HOMEBOY_CHANGED_TEST_FILES for scoped test runs
- emit type_names in fingerprint output
- language grammar files for structural regex engine
- test output parsers for baseline ratchet and failure analysis
- write test results JSON for homeboy test baseline
- add code coverage collection to test runners
- annotation sidecar JSON for CI inline review
- add test_mapping config to Rust and WordPress extensions
- extract methods inside impl blocks and test modules
- add lint and test scripts for CI support
- add propagate_struct_fields refactor command
- structural hashes in fingerprint for near-duplicate detection
- emit method_hashes in fingerprint for duplication detection
- Rust refactor script for extension-powered refactor move
- Add fingerprint scripts for Rust and WordPress extensions
- add cleanup_paths to wordpress, nodejs, and rust modules
- make GitHub and crates.io publish scripts idempotent
- add package/publish module actions

### Changed
- extract bridge framework, delete stale runtime helpers
- rust v1.9.0
- rust extension v1.8.0 — decompose import resolution fixes
- split monolithic refactor.py into package
- share runner step filtering
- resolve conflicts with main (PR #82 results parsers)
- Add dead code fields to Rust and WordPress fingerprint scripts
- Add feature_context rules and richer templates to Rust and WordPress extensions
- Add audit doc config (feature_labels, doc_targets) to WordPress and Rust extensions
- rename modules to extensions across repo
- migrate all module manifests to grouped capability structure
- convert module configs to snake_case keys
- Initial commit

### Fixed
- make HOMEBOY_FIX_ONLY the single auto-fix contract for all runners (#1145)
- resolve decompose imports by child ownership
- harden rust autofix import resolution
- decompose fixer visibility upgrade and trait import carry-over
- distinguish test functions from test helpers in fingerprinting
- unit test template should not assert is_ok() on () return
- expand Rust visibility matching to include pub(super), pub(self), pub(in ...)
- seed changelogs with current versions for homeboy release
- comprehensive decompose import resolution — trait imports, super:: paths, doc comments
- decompose import resolver now detects functions, constants, and glob imports
- scope cargo fmt --check to changed files in CI
- recognize Rust shorthand field init syntax
- normalize test method names in fingerprint and add skip_test_patterns
- improve bodyless trait method detection for unused parameter analysis
- skip unused parameter detection for trait method declarations
- eliminate unused_parameter false positives from type path segments
- narrow cfg(test) module detection
- handle rust lifetimes in boundary parsing
- avoid duplicate failure output replay in rust runner
- add preflight checks for cargo-dist and jq in package.sh
- add_pub_crate skips doc comments and attributes when finding declaration keyword
- Fix Rust extension feature_labels keys for substring matching
- fixed wordpress build script to handle nested blocks, also updated modules providing cli tools to support direct execution
- fixing bugs and updating things
- broadcast action pattern and path quoting

## [1.9.0] - 2026-03-21

### Added
- Initial independent component release
