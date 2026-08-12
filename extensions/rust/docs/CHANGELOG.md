# Changelog

All notable changes to the **rust** extension will be documented in this file.

## [1.34.1] - 2026-08-12

### Fixed
- make a widened changed-test selection attributable
- leave durable evidence when a test shard is truncated

## [1.34.0] - 2026-08-12

### Added
- run sharded tests from a prebuilt nextest archive

## [1.33.3] - 2026-08-09

### Fixed
- classify Cargo inventory outcomes

## [1.33.2] - 2026-08-09

### Fixed
- scope shard inventory to changed tests

## [1.33.1] - 2026-08-09

### Fixed
- count terminal nextest outcomes [AI: OpenAI gpt-5.6-sol via OpenCode]

## [1.33.0] - 2026-08-09

### Added
- resolve changed test identity unions

### Fixed
- separate Cargo workspace and component roots
- resolve selection paths from component root
- derive changed package from Cargo metadata
- version changed selection artifact contract
- replay changed selection without shard manifest

## [1.32.11] - 2026-08-06

### Fixed
- ignore non-object nextest records

## [1.32.10] - 2026-08-06

### Fixed
- bind shard authority to filtered nextest plan

## [1.32.9] - 2026-08-06

### Fixed
- account for planned ignored shard tests

## [1.32.8] - 2026-08-06

### Fixed
- reconcile nested ignored nextest events

## [1.32.7] - 2026-08-05

### Fixed
- inventory test-profile artifacts

## [1.32.6] - 2026-08-05

### Fixed
- isolate nextest list JSON

## [1.32.5] - 2026-08-05

### Fixed
- batch nextest shard manifests

## [1.32.1] - 2026-08-04

### Fixed
- structure toolchain readiness probes

## [1.32.0] - 2026-08-04

### Added
- add deterministic test shard manifests

## [1.31.2] - 2026-07-30

### Fixed
- run workspace member tests on every scope kind

## [1.31.1] - 2026-07-30

### Fixed
- reuse gh auth for Homebrew publishing

## [1.31.0] - 2026-07-29

### Added
- declare lint toolchain readiness

### Fixed
- seed the declared lint.findings sidecar on clean runs

## [1.29.5] - 2026-07-28

### Changed
- drop retired manifest keys and dead validate scripts

## [1.29.4] - 2026-07-26

### Fixed
- preserve toolchain homes for isolated gates
- widen to the full suite when a derived scope runs no tests

## [1.29.3] - 2026-07-26

### Fixed
- produce Cargo gate diagnostics

## [1.29.0] - 2026-07-21

### Added
- emit policy-flow fingerprints

## [1.27.0] - 2026-07-20

### Added
- add find_definition refactor command for struct discovery

## [1.26.0] - 2026-07-20

### Added
- add collapse_struct_defaults — inverse of propagate

## [1.25.0] - 2026-07-17

### Added
- add extension discovery composition metadata

### Changed
- share runner sidecar adapter loading

## [1.23.10] - 2026-07-16

### Fixed
- run full-scope tests across the whole workspace

## [1.23.9] - 2026-07-15

### Fixed
- retry crates.io publication rate limits

## [1.23.8] - 2026-07-15

### Fixed
- publish Rust workspace crates in dependency order

## [1.23.7] - 2026-07-12

### Fixed
- Fix #2226: retire Homeboy compat surfaces

## [1.23.6] - 2026-07-07

### Changed
- Declare source sync excludes in manifests

## [1.23.4] - 2026-07-04

### Changed
- Purge WP Codebox Data Machine plumbing

## [1.23.3] - 2026-07-04

### Changed
- Converge shell runners on shared harness

## [1.23.2] - 2026-07-04

### Changed
- Consolidate runtime helper smoke coverage

## [1.23.1] - 2026-07-01

### Changed
- Declare extension contract producers

## [1.22.3] - 2026-06-27

### Changed
- drop dead per-language runtime helper shims

## [1.22.2] - 2026-06-27

### Changed
- Single-source shared shell runner libs to top-level scripts/lib

## [1.22.1] - 2026-06-26

### Changed
- Remove generated local artifacts

## [1.22.0] - 2026-06-21

### Added
- publish Homebrew formulae from rust releases

### Fixed
- remove shared core helper fallbacks
- prefer runtime helpers in extension wrappers
- consume core test scope in runners

## [1.21.1] - 2026-06-15

### Fixed
- stabilize Rust release gates

## [1.21.0] - 2026-06-15

### Added
- add Rust toolchain acceleration knobs

### Fixed
- speed up Rust test targeting

## [1.20.8] - 2026-06-06

### Changed
- Share runtime helper fallbacks

## [1.20.7] - 2026-06-05

### Changed
- Declare Rust test filter passthrough mapping

## [1.20.6] - 2026-06-02

### Fixed
- consume core bash preflight helper

## [1.20.5] - 2026-05-30

### Fixed
- keep runner prelude self-contained

## [1.20.4] - 2026-05-30

### Changed
- use typed sidecar writer APIs

## [1.20.3] - 2026-05-30

### Changed
- Use shared runner prelude in extension runners

## [1.20.2] - 2026-05-30

### Changed
- Normalize extension fix result capture

## [1.20.1] - 2026-05-30

### Changed
- Use core bench scenario writer in Rust runner

## [1.20.0] - 2026-05-30

### Added
- consume manifest changed-test routing

## [1.19.5] - 2026-05-30

### Changed
- Add shared command capture helper

## [1.19.4] - 2026-05-30

### Changed
- Add generic settings accessors
- Use core helper for annotation sidecars
- share test result parser adapters

## [1.19.3] - 2026-05-30

### Fixed
- Fix Rust runner step fallback

## [1.19.2] - 2026-05-30

### Changed
- Use core sidecar helpers in non-WP runners

## [1.19.1] - 2026-05-29

### Fixed
- provide shared Rust target env

## [1.19.0] - 2026-05-29

### Added
- add rust bench adapters

## [1.18.0] - 2026-05-29

### Added
- capture rust bench phase timings

## [1.17.6] - 2026-05-27

### Changed
- share bash version preflight

## [1.17.5] - 2026-05-22

### Fixed
- make coverage completeness audit advisory

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
