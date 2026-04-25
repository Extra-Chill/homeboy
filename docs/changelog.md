# Changelog

All notable changes to Homeboy CLI are documented in this file.

(This file is embedded into the CLI binary and is also viewable via `homeboy changelog`.)

## [0.90.1] - 2026-04-25

### Fixed
- gate service supervisor behind #[cfg(unix)] so Windows builds

## [0.90.0] - 2026-04-25

### Added
- gate autofix by finding confidence
- add --rig flag for rig-pinned benchmarking (closes #1466)
- add verification phase contract
- introduce rig primitive for local dev environments (Phase 1, closes #1461)
- add Bench capability — sibling of Lint/Test/Build with p95 regression ratchet
- --footer / --footer-file on sectioned pr comment (closes #1470)
- --path flag on git issue/pr subcommands for unregistered checkouts
- sectioned PR comment primitive (closes #1348)
- shared-scaffolding detector (closes #1272)
- dead-guard detector (closes #1270)
- deprecation-age detector (closes #1271)
- GitHub issue and PR primitives on `homeboy git`
- repeated-literal-shape detector (closes #1274)
- add component.not_attached error for registered-but-unattached components
- auto-create GitHub Release after tag push
- post-write verify gate for automated refactor (#1167)
- add --fetch flag for upstream drift detection

### Changed
- chmod +x topology script in test_run_topology_script
- pass expected-commands to stop orphan-reconciliation churn
- complete changelog automation transition
- collapse pipeline/executor/resolver into straight-line script
- extract shared FileStateEntry primitive for both undo paths

### Fixed
- refresh baseline on top of #1490 (post-#1491 rebase)
- refresh baseline after #1487, #1480, #1468, #1385 merges
- split inline test methods from production methods in fingerprint (closes #1471)
- resolve three post-rig findings + refresh baseline, format main
- collapse backslash escapes in replacement templates
- invoke extension scripts directly to respect shebangs
- release pipeline errors that teach — explicit versions, recovery hints
- treat non-git install dirs as clean on update
- restore test_script() and test_mapping() accessors
- auto-init missing changelog on first release
- preserve nested HashMap lookup keys in merge_config normalization

## [0.89.1] - 2026-04-21

### Fixed
- fail fast on dirty tree before lint, ignore homeboy scratch (#1162)

## [0.89.0] - 2026-04-21

### Added
- project-scoped cli_path with Studio auto-detect (#1165)

## [0.88.11] - 2026-04-20

### Fixed
- reject intra_method_duplicate auto-removal inside open expressions (#1164)

## [0.88.10] - 2026-04-20

### Fixed
- suppress unreferenced_export false positives on hook callbacks and same-file references (#1149)

## [0.88.9] - 2026-04-20

### Fixed
- align dry-run files_modified with what --write actually applies (#1159)

## [0.88.8] - 2026-04-18

### Changed
- Remove pure planner artifacts (Phase 5a of #1041)
- Phases 3-6: RefactorPrimitive cleanup, EditOp alignment, serde reporting (#1041)
- Wire propagate command through apply_edit_ops()
- Clean up stale doc comments referencing removed apply chain
- Remove legacy InsertionKind dispatch chain — 1,500+ lines of dead code
- Wire apply_edit_ops() into the fixer pipeline
- cargo fmt on edit_op_apply.rs
- Revert "chore(ci): homeboy autofix — refactor (1 files, 15 fixes)"
- Split apply logic into edit_op_apply.rs to fix audit GodFile/HighItemCount
- Add apply logic for EditOp — resolve_anchor, apply_edit_ops_to_content, apply_edit_ops

### Fixed
- surface PHPUnit/runner stdout+stderr on test failure (#1143)
- component create auto-detects changelog path (#1128)
- deploy cluster — component set id round-trip + non-git local_path (#1140, #1141)
- send HOMEBOY_FIX_ONLY=1 to the extension in lint/test fix stages (#1145)
- auditor false-positives for namespaces, imports, unused params (#1134, #1135, #1136)
- deploy trio — resolver, Studio cli_path, --head branch warning (#1137, #1138, #1139)
- discover standalone components from ~/.config/homeboy/components/ (#1131)
- filter release/version-bump commits from changelog and auto-discover changelog path (#1127, #1128)
- Fix orphaned test false positives on behavioral/scenario test names

## [0.88.7] - 2026-04-08

### Fixed
- replace cron with push trigger for release workflow

## [0.88.6] - 2026-04-07

### Fixed
- skip import insertion when alias collides with existing import

## [0.88.5] - 2026-04-04

### Fixed
- raw string awareness in orphaned test brace counter

## [0.88.4] - 2026-04-04

### Changed
- refactor engine owns all fix application — lint + test

## [0.88.3] - 2026-04-04

### Fixed
- validate trait extraction — PSR-4, body comparison, line validation

## [0.88.2] - 2026-04-03

### Changed
- fix formatting in version_overrides.rs

### Fixed
- always pass --allow-root to WP-CLI in WordPress deploy

## [0.88.1] - 2026-04-02

### Changed
- Extract skill to standalone homeboy-skills repo
- Update audit baseline after file-level components merge
- Add file-level component deploy strategy
- Remove .homeboy-build-meta.json sidecar — one homeboy file per repo

### Fixed
- apply cargo fmt formatting for release
- ScopedExtensionConfig captures flat extension settings

## [0.88.0] - 2026-03-31

### Added
- add autofix safety guards to refactor --write pipeline
- homeboy upgrade now updates all extensions including linked ones
- make component ID optional for audit/lint/test/refactor/scaffold
- add --git-identity flag for CI bot commits
- skip lint/test stages when cached output shows clean pass
- add --commit and --git-identity flags
- resolve shallow clone ancestry for --changed-since diffs

### Fixed
- resolve Rust 2021 reserved prefix compile errors blocking release
- remove dead fields to eliminate all compiler warnings
- auto-detect deploy ownership from parent directory instead of target
- three bugs — CSS in rename, stale deploy, release --deploy skip

## [0.87.1] - 2026-03-28

### Changed
- Apply cargo fmt formatting
- Move raw string detection to grammar engine, fix orphan heuristic
- Add build provenance tracking and deploy warning for unreleased commits
- Add EditOp conversions for propagate and transform commands
- Add shared EditOp enum — canonical vocabulary for file edits
- Migrate comment_fixes and intra_duplicate_fixes to use builder helpers
- Promote orphaned test deletion when source file is deleted
- Promote more intra-method duplicates to automated fix

### Fixed
- Fix two autofix false positives exposed by CI run
- Fix orphaned test fixer matching functions inside string literals

## [0.87.0] - 2026-03-28

### Added
- support multi-component refactors and root-aware artifact deploys
- streamline gate-refactor — apply fixes directly, no dry-run
- refactor --from audit reads cached output when available
- add RunDir orchestration contract for pipeline step I/O

### Changed
- simplify source-driven automation flow
- Refactor scaffold/autofix boundaries and remove safe-plan gating
- replace inline release autofix with homeboy-action
- --bump directive + BREAKING CHANGE body detection

### Fixed
- remove 3 broken auto-generated test stubs in git::changes
- apply cargo fmt to resolve lint failures
- fail release when changelog target stays stale
- harden release packaging and unsafe autofix paths
- scope refactor generation before planning fixes
- keep decompose from extracting invalid root fragments
- use engine symbol data in export and duplicate fixers
- harden autofix structural parsing and import application
- unblock release by applying rustfmt cleanups
- stabilize audit signature mismatch baselines
- decompose algorithm quality — 4 fixes for cross-fixer conflicts and item extraction
- improve autofix fixer quality — prevent 4 classes of broken output
- use positional arg for extension install in release workflow
- always rebuild before deploy — remove stale artifact reuse (#991)
- import_add fixer skips locally-defined symbols
- apply cargo fmt to resolve CI lint failures
- revert broken autofix decompose (second occurrence)
- revert broken autofix decompose, add clean-tree guard for refactor

## [0.86.2] - 2026-03-24

### Changed
- remove sandbox from refactor planner, operate on working tree

## [0.86.1] - 2026-03-24

### Fixed
- merge same-file fixes before applying to prevent brace corruption

## [0.86.0] - 2026-03-24

### Added
- directory-based config hierarchy — fleet, project, and component levels
- add --context filter to refactor rename
- add hoist_static transform context + convert Regex to LazyLock
- richer type introspection for test generation — field-level assertions
- add --user override and per-server env to fleet exec/ssh
- add repeated struct field pattern detection
- add shadow module detection audit rule
- expand intra-method duplicate fixer to handle non-adjacent blocks
- add autofix for legacy_comment and near_duplicate findings
- detect error propagation branches from ? operator
- async function test generation + is_async on TestPlan
- surface remote_owner in component list + warn on WordPress deploy
- component create --project flag and next-step hints
- wrapper-to-implementation inference + fix broken auto-generated tests
- enhance fleet status with observability dashboard (#613)
- batch version bump for multiple components (#917)
- project-scoped status dashboard with version drift view

### Changed
- remove broken autofix decompose of grammar.rs
- add concurrency group to cancel stale PR runs
- remove all internal validation from refactor — let CI handle it
- remove convergence loop from refactor command — single pass only
- hub-aware decompose grouping to prevent mega-clusters
- Add compile check between planner stages to skip broken cascades
- Fail-fast on compile errors in convergence loop
- serialize auto-refactor across overlapping release runs

### Fixed
- truncate decompose module names to 3 meaningful words
- restore branch after deploy tag checkout + use dirname for remote path
- skip intra-duplicate removal when block has unbalanced delimiters
- fast brace-balance check before expensive sandbox compile
- remove dead code from test_gen_fixes.rs
- skip dead_code removal in test modules + fix 4 pre-existing test failures
- prevent test gen from producing broken code + remove broken auto-generated tests
- sanitize condition text in test template variables + cargo fmt
- rewrite comment fixer to remove legacy code blocks, not just comments
- handle multi-line function signatures in contract extraction
- deduplicate test function names in render_test_plan
- fall back to simple assertions when test gen can't resolve value types
- generate proper PHP imports in duplicate function fixer
- deploy from clean tag clones — preserve remote_path, inherit extensions, stabilize component IDs
- auto-refactor should run even when quality gates fail
- sanitize condition text in generated test assertions

## [0.85.3] - 2026-03-22

### Fixed
- correct depth check in find_function_body_range for contract extraction

## [0.85.2] - 2026-03-22

### Changed
- fix rustfmt formatting in planning.rs

### Fixed
- deploy robustness — skip unrelated components, force after bump, warn on failure
- use serde snake_case for fixability by_kind keys
- use autofix: false instead of autofix-mode: disabled
- fall back to rustfmt when cargo fmt fails in sandbox

## [0.85.1] - 2026-03-21

### Fixed
- make audit and lint read-only in release pipeline
- scope monorepo releases to component subdirectories
- escape inner double quotes in generated test assertion strings
- build before tag and reflect deploy failures in JSON envelope

## [0.85.0] - 2026-03-21

### Added
- inline test module placement instead of orphaned test files (#818)
- method receiver construction for impl block methods (#818)
- cross-file type registry for project-wide struct resolution (#818)
- configurable field capture groups for PHP class property parsing (#818)
- auto-fix adjacent intra-method duplicates
- struct introspection for field-level test assertions
- wire behavioral inference into test generation pipeline

### Changed
- remove all language-specific code from core test generation

### Fixed
- drop PlanOnly fixes in write mode to avoid wasted CI work
- skip lint smoke for Safe-tier fixes in chunk verification
- exclude build artifacts from refactor sandbox
- three root causes blocking autofix write path (#818)
- format files after write in apply_fixes_chunked before lint smoke
- format sandbox between refactor stages so lint sees clean code

## [0.84.0] - 2026-03-20

### Added
- auto-fix HighItemCount findings via decompose pipeline

## [0.83.0] - 2026-03-20

### Added
- include fixability metadata in full audit output

### Changed
- pre-1.0 semver — breaking changes bump minor, not major
- remove changelog add command — release owns all changelog entries
- remove --fix from audit/lint/test — refactor owns all code changes

## [0.82.0] - 2026-03-19

### Added
- add failure-trap and write-test-results runtime helpers
- compiler warning autofixer — auto-remove unused imports, dead code, unused mut
- language-configurable param format and return type separator for contract extraction
- generate tests for MissingTestMethod findings
- wire test generation into audit fix pipeline

### Fixed
- add missing newline to empty test file to unblock release
- extract duplicate test helpers to shared module
- preserve array/object values in extension settings JSON
- defer artifact cleanup when --deploy follows release
- rename orphaned tests and update baseline to unblock release
- string-aware brace counting in find_test_function_range

## [0.81.1] - 2026-03-17

### Fixed
- const/static boundary detection handles multi-line array initializers (#841)

## [0.81.0] - 2026-03-17

### Added
- parameter removal autofix for truly unused params (#824 Phase 2)
- call-site-aware unused parameter detection (#824)
- add `homeboy validate` CLI command

### Fixed
- validate_write on all refactor write paths (#832)
- decompose mod.rs targets use parent dir, not mod/ subdir (#832)

## [0.80.0] - 2026-03-17

### Added
- refactor transform — case transforms, multi-line matching, docs
- add `homeboy version bump` as alias for `homeboy release`
- auto-discover misplaced tests + FileMove autofix
- orphaned test fixer tries rename before deletion
- test_templates in ContractGrammar + end-to-end generate_tests_for_file
- test plan generator — contract to test cases, template-rendered
- FunctionContract — language-agnostic code comprehension primitive
- run project formatter after refactor --write applies code

### Fixed
- version show warns on non-matching targets instead of silently dropping
- auto-resolve remote_path at component resolution time
- post-release hooks run even with --skip-publish

## [0.79.0] - 2026-03-17

### Added
- naming pattern convention detection for parallel filtering
- cross-file frequency filter for parallel detection
- include cross-directory convention methods in parallel detection filter
- convention-aware parallel implementation detection
- audit reference dependencies — include framework source in cross-reference analysis
- surface compiler warnings as audit findings
- post-write compilation validation gate
- post-write compilation validation gate for all code-modifying commands
- expose fixability counts in standard audit output
- deploy from GitHub release artifacts
- centralize execution context resolution
- deploy from GitHub release artifacts — skip local builds when remote_url is set
- centralize execution context resolution for lint/test/build/audit/refactor commands

### Changed
- cargo fmt — fix formatting from autofix commits
- batch verification for audit --fix --write
- unify convention method set, apply to duplicate detection
- merge SafeAuto + SafeWithChecks into single Safe tier
- retrigger with updated extension import resolver v2
- Revert "chore(ci): homeboy autofix — refactor [duplicatefunction, orphanedtest] (11 files)"
- consolidate 11 directory walkers into codebase_scan

### Fixed
- exclude test lifecycle methods from convention expectations
- audit autofix generates real method bodies from conforming peers
- skip parallel findings when either method is convention-expected
- skip unreferenced export check for test files
- reference fingerprints must not be checked for dead code themselves
- prevent homeboy.json corruption during component operations
- repair broken autofix decompositions — imports, module names, doc comment placement
- decompose slug sanitization — hyphens produce invalid Rust module names

## [0.78.0] - 2026-03-15

### Added
- refactor move --file for whole-module relocations

### Changed
- organize refactor module + fix decompose re-exports

### Fixed
- pub use removal preserves trailing commas + treat decompose side-effects as cascading
- enable lint autofix in release pipeline + format today's code
- signature mismatch only flags isolated signatures, not variant families
- exclude CHANGELOG from docs audit by default
- remove .homeboy/audit-rules.json dual path — homeboy.json is the single source (#779)
- lint envelope trust + component path resolution (#696, #694)

## [0.77.0] - 2026-03-15

### Added
- two-step unreferenced_export fixer — narrow visibility + remove re-exports

### Changed
- move undo/ into engine/ (cross-cutting infrastructure)
- relocate audit.rs core tests to their library modules
- extract extension aggregate queries to core
- add UndoSnapshot::capture_and_save, collapse 4 duplicates
- extract deploy orchestration to core (#764, fleet automation)
- consolidate binary crate sprawl into commands/
- remove command-layer cruft (test_scope.rs, refactor_tests.rs)
- move local_files into engine/ (filesystem utility)
- unify Build execution with ExtensionRunner contract
- Resolve core/ homeless files and file-directory collisions

### Fixed
- resolve_execution_context now handles Build capability (#764)
- repair stale doc reference fixer and eliminate example-path false positives
- exclude pub(crate) functions from public_api

## [0.76.2] - 2026-03-14

### Fixed
- prevent skip list from suppressing calls to defined functions
- eliminate orphaned_internal false positives
- reduce false positives in unreferenced_export fixer guards

## [0.76.1] - 2026-03-14

### Changed
- Remove backward-compat shims and rehome homeless core modules
- reduce false positives in parallel_implementation and orphaned_test detectors

### Fixed
- eliminate unused_parameter false positives for trait methods
- prune stale refactor sandbox directories on startup

## [0.76.0] - 2026-03-13

### Added
- add orphaned test removal fixer — 709 new fixable findings

## [0.75.1] - 2026-03-13

### Changed
- remove scaffold test stub generation from autofix pipeline

## [0.75.0] - 2026-03-13

### Added
- detect orphaned test methods referencing deleted source symbols
- add generic command scope exclusions
- autofix simple broken doc references
- autofix stale doc references from audit
- add explicit fix planning sidecars

### Changed
- move auto-refactor to post-release so it never blocks releases
- run cargo fmt to fix import ordering across 44 files
- extract propagate business logic from commands/ to core/refactor/propagate.rs
- extract docs business logic from commands/ to core/
- remove standalone docs audit subcommand and dead code
- promote undo.rs to undo/ directory with snapshot + rollback split
- promote db.rs to db/ directory (operations + tunnel)
- extract transfer business logic from commands/ to core/
- convert upgrade from include! fragments to proper modules
- move ssh module into server/
- remove keychain module and keyring dependency
- promote server.rs to server/ module directory
- extract audit workflow and report into core
- consolidate compute_changed_test_files into single source of truth
- extract lint workflow and report into core
- extract test scaffold and report into core
- move project report shaping into core
- thin project command wrappers
- hide deprecated init surface
- move init report under context
- remove cleanup command surface
- establish core fleet modules
- extract fleet status module
- convert component into module directory
- extract component inventory and mutations
- extract component versioning helpers
- extract component relationship helpers
- extract component resolution module
- extract portable component module
- collapse runner preparation into execution
- move runner context setup into execution
- share extension execution plumbing
- merge main into project files branch
- move files under project
- move logs under project
- establish core project modules
- remove final utils module
- move command and core helpers into owned modules
- make project attachments explicit
- remove component local config support
- derive component inventory
- Merge origin/main into refactor/centralize-effective-component-resolution
- reuse effective component resolver
- adopt central component resolver
- centralize effective component resolution
- make component init repo-first
- remove project component registry dependency
- clean up project attachment resolution
- make project attachments canonical
- use project attachments in build
- use project attachments in context and init
- let projects attach repo-backed components
- remove legacy build command support
- type project component overrides
- move build into extension domain
- move shared primitives into engine
- move codebase scan into engine
- move grammar into extension core
- thin release command
- move runtime temp paths into engine
- move lint baseline into extension domain
- move changelog into release domain
- move version into release domain
- move test domain and symbol graph
- align test lint audit and scaffold domains
- refactor(code-factory): split plan generation modules
- refactor(code-factory): remove code_audit fixer module
- refactor(code-factory): derive signatures from grammar symbols
- refactor(code-factory): move test helper parsing into plan
- refactor(code-factory): move apply content engine into refactor auto
- refactor(code-factory): move fix helpers into refactor plan
- refactor(code-factory): extract refactor plan and auto modules
- refactor(code-factory): auto-fix namespace declarations
- refactor(code-factory): auto-fix missing interface conformance
- refactor(code-factory): centralize lint and test fix requests
- refactor(code-factory): route audit source writes through refactor core
- refactor(code-factory): centralize detector-triggered refactor plumbing
- unify build around resolved extension context
- make extension runner context-only
- remove transitional extension command helpers
- let runner consume execution context
- introduce extension execution context
- make extension resolution capability-based
- centralize extension script resolution
- Revert "fix(ci): route PR and release workflows through homeboy-ci"
- make refactor source-driven
- move CI autofix into refactor phase

### Fixed
- allow synthetic component when --path provided without homeboy.json
- rewrite signature tests from PHP to Rust and fix strip_return_type paren matching
- resolve 6 pre-existing test failures blocking release
- add autofix-commands to release audit gate — was enabled but had no commands to run
- add .release-last-failed to .gitignore — unblocks continuous release
- restore --lib test compilation and add audit report/run tests
- use homeboy release dry-run for gating
- scope failed-attempt cache to current ref
- re-export planner helpers for tests
- remove final refactor ci workflow references
- use source-driven refactor autofix flow
- run refactor autofix after failing PR checks
- route PR and release workflows through homeboy-ci
- reduce planner audit surface
- track sandbox changes without git metadata
- update refactor docs and split command tests
- preserve impl blocks in type buckets

## [0.74.1] - 2026-03-09

### Changed
- remove PHPUnit-specific code and unify is_test_path

## [0.74.0] - 2026-03-09

### Added
- make baseline ratchet opt-in via --ratchet flag

### Fixed
- deduplicate levenshtein and module_path_from_file

## [0.72.0] - 2026-03-08
### Added
- enable autofix on all CI jobs + release autofix PRs
- auto-detect bump type from conventional commits
- auto-ratchet baseline after audit --fix --write resolves findings
### Fixed
- decompose rollback now covers caller files, unify snapshot systems
- repair broken imports from squash merge of feat/auto-release
- separate test files from convention groups and normalize signatures

## [0.71.1] - 2026-03-07
### Fixed
- improve test coverage precision with visibility filtering and skip patterns (#577) (audit)

## [0.71.0] - 2026-03-07
### Added
- add server health metrics to fleet status (#575) (fleet)

## [0.70.0] - 2026-03-07
### Added
- persistent undo command for write operations (#573)

## [0.69.0] - 2026-03-07
### Added
- recursive convergent autofix with decompose primitive (#572) (audit)

## [0.68.0] - 2026-03-07
### Added
- call-site impact tracing for scoped audit (#564) (#565) (audit)
### Fixed
- scoped audit exits 0 when no baseline exists anywhere (#563)

## [0.67.0] - 2026-03-07
### Added
- differential CI — only fail on findings introduced by the PR (#562)
- add version undo command (#406) (#553)
- smart decompose grouping with 5-phase semantic clustering (#552) (refactor)
- expose core runner helper (#517) (extension)
### Fixed
- deploy from latest tag by default, fleet status checks live versions (#561)
- reduce helper-file precision noise (#515) (audit)

## [0.66.0] - 2026-03-07
### Added
- grammar-driven parse_items in core + pre-write validation + god file threshold (#551)
### Fixed
- strip generated code from --fix JSON output by default (#549) (audit)

## [0.65.1] - 2026-03-07
### Fixed
- ExtensionRunner falls back to portable config when component not registered (#550)

## [0.65.0] - 2026-03-07
### Added
- autofix unreferenced exports with visibility narrowing (#548) (audit)
### Fixed
- tighten visibility for 51 unreferenced exports (#547) (audit)

## [0.64.0] - 2026-03-07
### Added
- expand autofix coverage for inline tests and placeholder scaffolds (#532) (audit)
- detect duplicated code blocks within methods (#531) (audit)

## [0.63.0] - 2026-03-06
### Added
- cron-triggered continuous release workflow (#530)

## [0.62.1] - 2026-03-06

### Fixed
- changelog dedup, --skip-publish flag, remove redundant cargo publish hook (#528)

## [0.62.0] - 2026-03-06

### Added
- enable audit autofix on PRs and release pre-gate (#527)

### Fixed
- version bump dry-run no longer mutates changelog or bypasses lint baseline (#526)

## [0.61.0] - 2026-03-06

### Added
- standardize component resolution for release and audit commands (#525)

## [0.60.0] - 2026-03-06

### Added
- add convergent autofix engine (#514)

### Fixed
- load portable extensions (baselines, extensions config) for path-based lint/test commands in CI
- add missing source file in dedupes_missing_test_file_creation test
- load portable extensions for path-based commands (#519)
- repair 0.59.0 release notes

## [0.59.0] - 2026-03-06

### Added
- add audit impact projection for refactor decompose plans

### Changed
- decompose the upgrade core into include fragments for a smaller, more modular implementation

### Fixed
- add targeted rename controls for safer refactor test migrations (#503)
- dedupe decompose items before grouped moves
- keep decompose tests black-box while removing unused exports
- Fixed scoped audit exit codes to ignore unchanged legacy outliers in changed-since runs
- restore crates.io publishing in release CI

## [0.58.1] - 2026-03-06

### Fixed
- add portable component id for action validation

## [0.58.0] - 2026-03-06

### Added
- add extension-driven test topology policy

### Changed
- cover topology helpers and script execution
- track homeboy-action via v1 tag (#502)

### Fixed
- restore legacy hook wording and tune marker matching
- resolve release-gate drift and rebaseline

## [0.57.0] - 2026-03-05

### Added
- detect legacy and stale comment markers (#500)
- runner step contract and generic output parser substrate (#499)
- add configurable layer ownership rules (#498)
- add structured findings baseline contract (#497)
- add capability probes and CI-focused JSON summaries (#496)
- feat(test/docs): support path-first runs in dogfood workflows (#495)
- add shared --fix outcome primitive (#493)
- detect directory sprawl hotspots (#486)
- block under-bumped version bumps by default (#481)
- add decompose planning mode for large-file refactors (#476)
- feat(audit fix): scaffold missing test methods as ignored TODO tests (#480)
- feat(audit fix): scaffold missing test files from coverage findings (#479)

### Changed
- pin homeboy-action to v1.1.1 across workflows (#494)
- cover resolve_binary_on_path lookup (#401)

### Fixed
- correct --analyze aggregate totals when parser omits counts (#484)

## [0.56.1] - 2026-03-05

### Added
- include-fragment coverage handling and bulk JSON recipes (#468)
- changed-since impact-scoped test execution (#448)

### Changed
- decompose core deploy orchestration into focused modules (#458)

### Fixed
- include explicit ids in component list output (#455)
- honor flag-only project/component args and improve selection errors (#454)
- support [Next] alias for unreleased section (#456)
- fix(test-drift): tighten safe literal token filtering for auto-fix
- prevent homeboy flags leaking into test runners (#446)
- remove duplicate Next section from changelog

## [0.56.0] - 2026-03-04

### Added
- test scaffold — generate test stubs from source file conventions (#422)
- extract generic codebase scanner with variant discovery for refactor rename
- cross-separator variant generation and improved boundary detection for refactor rename
- scope audit to changed files with --changed-since flag (#416)
- test drift detection — cross-reference production changes with test files (#423)
- refactor transform — regex find/replace across codebases (#410)
- test failure analysis — cluster by root cause and suggest fixes (#421)
- test baseline ratchet — CI floor for pass/fail counts (#411)
- baseline/ratchet integration for docs audit and cleanup (#417)
- add generic baseline/ratchet primitive to utils (#413)

### Changed
- extract shared CLI arg groups via Clap flatten (#436)
- run audit job independently + fix formatting drift
- use homeboy-action with source build for audit + auto-issue

### Fixed
- resolve merge conflict between shared arg groups and test scaffold
- resolve Rust 1.93 clippy warnings and formatting drift
- build homeboy from source in CI audit instead of downloading release binary (#418)
- prevent release pipeline from publishing without binaries


## [0.55.0] - 2026-03-03

### Added
- add fleet exec — run commands across all projects via SSH

### Fixed
- docs audit now classifies example paths as Example confidence, not Unclear
- enable audit baseline comparison in CI — only fail on new drift
- hide --serial flag (reserved for future parallel mode) and fix description

## [0.54.1] - 2026-03-03

### Fixed
- fall back to two-dot diff when three-dot fails in shallow CI clones (#397)
- resolve upgrade panic by looking up binary on PATH instead of /proc/self/exe (#398)

## [0.54.0] - 2026-03-03

### Changed
- update audit baseline for v0.53.0 (459 findings, 70% alignment)
- add pre-release quality gate to release workflow

### Fixed
- deterministic duplication fingerprints for stable baselines (#394)
- make release step idempotent for cargo-dist v0.31.0

## [0.53.0] - 2026-03-03

### Added
- add --coverage and --coverage-min flags (#392)
- auto-pull and version verification before deploy (#381)
- add pre-release code quality gate (lint + test) (#375)
- add --path flag for CI-friendly path override (#379)
- add --changed-since <ref> flag for CI-friendly changed-file linting (#377)

### Changed
- remove docs scaffold subcommand (#389)
- Add structural test coverage gap detection (#373) (#388)
- Add dead code detection to audit pipeline (#384) (#387)
- add audit baseline ratchet — only fail on NEW findings (#383)
- add PR workflow with build/test + homeboy audit dogfooding (#380)

### Fixed
- fix build.rs raw string delimiter for docs with special characters (#390)
- fix release workflow cargo-dist version mismatch (#390)

## [0.52.1] - 2026-03-02

- fix(test): --path and --fix flags now correctly parsed by test command (#366)

## [0.52.0] - 2026-03-02

### Added
- Add refactor propagate subcommand for struct field propagation
- Add docs map command with mechanical markdown generation from source code
- Add deploy integration test suite with 29 tests covering safety chain, template rendering, and error messages
- Add planned/skipped counts to ProjectsSummary for accurate multi-project deploy reporting

### Changed
- Improve docs map output quality — module naming, cross-references, large module splitting
- Rewrite SKILL.md as agent bootstrap with discovery-first approach
- Add group size threshold, skip constructors, and dynamic namespace detection in code audit

### Fixed
- Add deploy safety guard — prevent deploying to shared parent directories (#353)
- Improve 'no components configured' error with actionable details and skipped component info (#329)
- Fix dry-run and check modes reporting 'deployed' status instead of 'planned' in multi-project deploys (#359)
- Improve fleet/multi-project deploy resilience — skip unknown projects instead of aborting

## [0.51.0] - 2026-02-28

### Added
- Add refactor rename command with case-variant awareness and word-boundary matching (#283)
- Add --literal mode for refactor rename — exact string matching without boundary detection (#299)
- Add collision detection in refactor rename dry-run — warns on duplicate identifiers and file conflicts (#292)
- Add snake_case compound matching in refactor rename — matches terms inside snake_case identifiers (#291)
- Add extension versioning with semver constraint matching (^, ~, >=, etc.) and auto-update checks on startup (#285)
- Add extension-powered language extractors — fingerprinting moved from built-in to extensions (#286)
- Add smart import detection for code audit — grouped imports, path equivalence, usage checking
- Add ImportAdd fix kind for auto-resolving missing import findings in code audit

### Changed
- Rename modules to extensions across entire codebase — CLI, config, docs, extensions repo (#284)
- Rename HOMEBOY_MODULE_PATH/ID env vars to HOMEBOY_EXTENSION_PATH/ID (#296)
- SKIP_DIRS (build, dist, target) only skipped at root level — nested dirs like scripts/build/ are now scanned (#297)
- Update README with new repo description, refactoring section, and extension versioning
- Normalize CmdResult type alias and dispatch pattern across all command modules
- Deprecate version set in favor of version bump (#259)

### Fixed
- Fix PHP method regex to handle multi-keyword modifiers in code audit
- Fix import regex to capture grouped imports correctly in code audit
- Fix false 'unconfigured version target' warning for already-configured PHP constants (#261)
- Fix version bump error messages to include field name and problem (#258)
- Handle cargo-dist subdirectory layout in upgrade script (#256)
- Clean target directory before archive extraction to prevent stale files (#257)
- Allow multiple version targets per file (#262)
- Surface post-release hook failures to stderr with non-zero exit code (#255)
- Normalize mut parameter modifier in signature comparison for code audit (#275)

## [0.50.1] - 2026-02-28

### Changed
- Replace scp -r with rsync for directory deploys (mirrors source exactly)

### Fixed
- Deploy uses rsync --delete to clean up stale files on target servers (#253)
- Detect local IPs on deploy to skip SSH when agent runs on the same server (#236)

## [0.50.0] - 2026-02-27

### Added
- Add code audit system with auto-discovery, convention detection, and drift analysis

- Add portable homeboy.json config with post:release hooks
- Add audit --fix with smart stub generation, naming/plural tolerance, and confidence filtering
- Add audit --baseline for drift comparison over time
- Add audit interface/trait compliance, cross-directory convention, signature consistency, and namespace/import detection

### Changed
- Suggest fix when version bump fails due to missing changelog target

### Fixed
- Fix version set --path not committing/tagging in correct repo
- Fix version set silently skipping changelog update
- Fix deploy artifact name mismatch with HOMEBOY_COMPONENT_ID env var

## [0.49.1] - 2026-02-25

### Changed
- Batch 3: remove 11 dead functions, narrow visibility across codebase
- Batch 2: unify ProjectsSummary, remove dead code, narrow visibility
- Batch cleanup: dead fns, to_details helper, serialize_with_id, deploy failed() constructor, visibility fixes
- Extract deploy_components() into focused single-concern functions
- Remove dead utility functions (~182 LOC)
- Remove --from-repo flag and build_from_repo_spec (~137 LOC)
- Standardize rename and delete_safe as universal entity primitives
- Adopt consistent logging with log_status! macro and to_json_string helper
- Replace all Error::other() escape hatches with specific error codes
- Add CWD auto-discovery for unregistered repos with homeboy.json
- Layer portable homeboy.json as live runtime defaults on component load
- Extract shared DynamicSetArgs processing, migrate project set
- Make global config writes atomic + warn on parse failures
- Replace production unwrap() calls with proper error handling (#192)
- Code quality sweep: consolidate duplicates, fix safety issues (#191)

## [0.49.0] - 2026-02-25

### Added
- Remote hook execution for post:deploy hooks via SSH with template variable expansion
- Extension dependency validation with actionable install error messages
- Path override flag for build, lint, test, and version commands
- Portable homeboy.json config for component creation from repo root

### Changed
- Improve extensions section in README and clarify local_path vs deploy target docs

### Fixed
- CLI create commands losing component id during serde serialization
- Extension install from monorepo URL creating ghost state

## [0.48.0] - 2026-02-25

### Added
- Cleanup command for config health checks (missing extensions, invalid paths, stale version targets)
- Startup update check with 24h cache notifies when newer version available
- Sibling section inference in docs generate auto-detects heading patterns from adjacent files
- Extension exec command for direct tool access without component context
- Replace @since placeholder tags during version bump
- Step and skip flags for extension run step filtering
- Docs audit supports direct filesystem paths without component registration
- Local flag on logs commands for agent/on-server mode
- Dedicated flags on component set for common fields

### Changed
- Extension manifests use nested capability groups (deploy, audit, executable, platform) — breaking JSON schema change
- Remove RawModuleManifest bridge (270 lines); capability structs deserialize directly
- General hook system replaces per-lifecycle hook executors (pre:version:bump, post:version:bump, post:release, post:deploy)
- entity_crud! macro generates standard CRUD wrappers, replacing per-entity output structs
- Remove Box::leak from dynamic extension CLI registration

### Fixed
- Entity set commands replace array fields by default instead of merging
- Lint changed-only passes absolute paths to extension runners
- Enable multiline mode for version target regex patterns
- Dynamic key-value flags on entity set commands fail with JSON parse error
- Fetch tags before baseline detection to prevent stale baseline_ref
- Skip redundant builds during deploy and detect self-deploy
- Swap ahead/behind parsing in remote_sync check
- Default to excluding CHANGELOG.md from docs audit

## [0.47.1] - 2026-02-23

### Changed
- Omit zero-value feature coverage fields from docs audit JSON output

### Fixed
- Filter Windows filesystem paths (e.g., AppData\Roaming) from class name extraction in docs audit
- Improve example context detection for 'this creates', 'would create', 'typically:' patterns

## [0.47.0] - 2026-02-23

### Added
- Add total_features and documented_features counts to docs audit summary for coverage reporting
- Include doc_context (surrounding lines) in broken reference output for faster remediation
- Add claim confidence classification (real/example/unclear) to docs audit, with code-block awareness and placeholder name detection

### Changed
- Rewrite docs audit action strings with source-of-truth framing (code is authoritative, docs must be updated to match)

## [0.46.0] - 2026-02-23

### Added
- Dedicated `status` command for focused, actionable component overview with filtering flags (`--uncommitted`, `--needs-bump`, `--ready`, `--docs-only`, `--all`) (#121, #119)
- `transfer` command supports local-to-remote (push) and remote-to-local (pull) in addition to server-to-server (#115)
- Post-deploy cleanup of build dependencies via extension-defined `cleanup_paths` and component `auto_cleanup` flag (#105)
- Configurable `docs_dir` and `docs_dirs` fields for component documentation audit
- Multi-directory docs scanning with automatic README inclusion
- `remote_owner` chown support in deploy for explicit file ownership

### Fixed
- `component set` now rejects unknown fields instead of silently dropping them; prevents false success when using `extension` (singular) instead of `extensions` (plural) (#124)
- Deploy command accepts component-only target like build command (#120)
- Double-escaped backslashes in version patterns are normalized at both parse and load time (#116)
- Audit feature patterns now scan all source files, not just changed ones
- Git-deploy components skip artifact resolution (#108)

### Improved
- Missing-extension errors on lint/test/build now include remediation hint: "Add a extension: homeboy component set <id> --extension <extension_id>" (#123)
- Init detects missing extension configuration as a config gap with auto-suggested extension type
- Clearer error message when changelog is not configured (#117)
- Usage examples added to `changelog add --help` (#118)

## [0.45.2] - 2026-02-17

### Fixed
- fix: allow git-deploy components without build artifacts

## [0.45.1] - 2026-02-17

### Added
- Undocumented feature detection in docs audit via extension audit_feature_patterns (#104)

## [0.45.0] - 2026-02-16

### Added
- Extension flag for component create and set (--extension)
- Auto-detect extension from component context in homeboy test

### Removed
- Fleet sync command deprecated — use homeboy deploy instead
- 800+ lines of hardcoded OpenClaw-specific sync logic removed from core

### Fixed
- docs_audit absolute path verification bug — Path::join with absolute paths bypassed source tree check

## [0.44.4] - 2026-02-16

### Fixed
- SSH non-interactive commands now use BatchMode, ConnectTimeout, and ServerAliveInterval to prevent hangs (#88)
- Version target patterns are validated at create time — rejects template syntax and missing capture groups (#90)
- component set now supports --version-target flag like component create (#91)

## [0.44.3] - 2026-02-15

### Fixed
- version bump: run pre_version_bump_commands after bump to keep generated artifacts (e.g. Cargo.lock) in the release commit
- deploy: upload to temp file + atomic mv to avoid scp 'Text file busy' when replacing running binaries

## [0.44.2] - 2026-02-15

### Fixed
- ssh: allow multi-arg non-interactive commands; improve non-TTY guidance

## [0.44.1] - 2026-02-15

### Fixed
- Update Cargo.lock after 0.44.0 release

## [0.44.0] - 2026-02-14

### Added
- Fleet sync command (homeboy fleet sync) — sync OpenClaw agent configs, skills, and tools across fleet servers with manifest-driven categories, JSON merging, auto-detection of OpenClaw home paths, ownership fixing, and dry-run support

## [0.43.1] - 2026-02-13

### Fixed
- Handle uncommitted changelog gracefully in version bump (#78)

- fix: scope --allow-root injection to wordpress extension only
- Better error message for missing unreleased changelog section
- Revert Stdio::null on git commands (broke HTTPS credential helper)

## [0.43.0] - 2026-02-13

### Added
- Support aliases for components, projects, and servers (#34)
- Detect and warn about outdated extensions in homeboy init (#26)
- Automatic retry with backoff for transient SSH failures (#51)
- Release --recover for interrupted releases (#38)
- Git-based deployment strategy (#52)

### Fixed
- Clarify local file permissions message with path and chmod modes (#9)
- Expand {{extension_path}} in project CLI command templates (#44)
- Fix environment-dependent docs audit test

## [0.42.0] - 2026-02-13

### Added
- support aliases for components, projects, and servers
- add transfer command for server-to-server file transfer (#67)
- add file download command (SCP remote-to-local)
- add class name detection to audit, fix scaffold false positives, document generate spec

### Fixed
- use non-existent path in docs audit test
- expand {{extension_path}} in project CLI command templates
- clarify local file permissions message with path and modes

## [0.41.2] - 2026-02-10

### Added
- Cross-compilation guide documenting platform requirements

## [0.41.1] - 2026-02-10

### Added
- OpenClaw skill for AI agent usage (skills/homeboy/)

## [0.41.0] - 2026-02-10

### Added
- Fleet management: create, list, show, delete, add, remove projects from fleets
- fleet status: check versions across all projects in a fleet
- fleet check: drift detection across fleet using deploy --check
- deploy --fleet: deploy component to all projects in a fleet
- deploy --shared: deploy to all projects using a component (auto-detect)
- component shared: show which projects use a component

## [0.40.4] - 2026-02-10

### Added
- Extension manifest: add Desktop runtime fields (dependencies, playwrightBrowsers, builtin actions)

### Fixed
- Parser: trim content in replace_all to match extract_all behavior (fixes version bump on files with trailing newlines)

## [0.40.3] - 2026-02-09

- Add cargo-dist release workflow for automatic homebrew tap updates

## [0.40.2] - 2026-02-09

### Added
- agnostic source directory detection for scaffold (#57)

## [0.40.1] - 2026-02-03

### Added
- add preflight remote sync check to version bump to prevent push conflicts

### Fixed
- source cargo env for source installs

## [0.40.0] - 2026-02-02

### Added
- filter merge commits from changelog auto-generation
- add --projects flag for multi-project deployment

## [0.39.5] - 2026-02-01

- inject --allow-root for root SSH deploy overrides

## [0.39.4] - 2026-01-31

### Added
- auto-inject --allow-root for root SSH users

## [0.39.3] - 2026-01-31

### Added
- support glob patterns in build_artifact

## [0.39.2] - 2026-01-31

### Added
- capture command output in JSON response

## [0.39.1] - 2026-01-28

### Added
- Display human-readable success summary after version bump/release
- Transform docs-audit from link checker to content alignment tool

## [0.39.0] - 2026-01-28

- add ValidationCollector for aggregated error reporting in version bump

## [0.38.6] - 2026-01-28

### Added
- validate conflicting version targets for same file
- add --fix flag for auto-fixing lint issues

### Fixed
- fix(docs-audit): filter false positives via extension-level ignore patterns

## [0.38.5] - 2026-01-28

- Fixing my fuck-up with version bumping

## [0.38.4] - 2026-01-28

- Make documentation guidance audit-driven with concrete commands

## [0.38.3] - 2026-01-28

- Stream test/lint output directly to terminal instead of capturing in JSON

## [0.38.2] - 2026-01-27

- Fix version bump race condition where changelog was finalized before all version targets were validated, causing 'No changelog items found' on retry after validation failure

## [0.38.1] - 2026-01-26

- Add flag-style aliases for version and changelog commands (#13, #32)

## [0.38.0] - 2026-01-26

### Added
- auto-generate changelog entries from conventional commits (#25)

## [0.37.5] - 2026-01-26

### Added
- Add --base64 flag to component/server set commands to bypass shell escaping (#24)

### Fixed
- Fix quote-aware argument splitting in normalize_args() for WP-CLI eval commands (#30)

## [0.37.4] - 2026-01-26

### Fixed
- Add --component option alias for changelog add (#32)

## [0.37.3] - 2026-01-26

### Fixed
- Graceful version bump when changelog already finalized for target version

## [0.37.2] - 2026-01-26

### Fixed
- Case-insensitive enum arguments for --type and BUMP_TYPE (closes #29)

## [0.37.1] - 2026-01-26

### Fixed
- Allow uncommitted changelog and version files during release (fixes #28)

## [0.37.0] - 2026-01-25

- Add configurable lint and test script paths via extension manifest (lint.extension_script, test.extension_script)

## [0.36.4] - 2026-01-24

### Removed
- Remove --force flag from version bump and release commands (bypassing validation defeats its purpose)

## [0.36.3] - 2026-01-23

- Add success_summary to pipeline output for human-readable release summaries

## [0.36.2] - 2026-01-23

- Fix error message visibility in internal_unexpected errors

## [0.36.1] - 2026-01-23

### Added
- Add changelog entry awareness to changes command

## [0.36.0] - 2026-01-23

- feat: distinguish docs-only commits from code changes in init command (#16)

## [0.35.1] - 2026-01-23

- Add clean working tree hint to changelog validation errors

## [0.35.0] - 2026-01-23

- feat: entity suggestion for unrecognized subcommands

## [0.34.1] - 2026-01-22

- fix: require clean working tree for version bump (removes pre-release commit behavior)

## [0.34.0] - 2026-01-22

- Add shared project/component argument resolution primitive (utils/resolve.rs)
- Add project-level build support with --all flag
- Support flexible argument order in changes command
- Add hooks system documentation
- Update agent system reminder wording

## [0.33.12] - 2026-01-22

- feat: add extension-defined CLI help configuration

## [0.33.11] - 2026-01-22

- fix: normalize quoted CLI args at entry point (closes #11)

## [0.33.10] - 2026-01-21

- Add post_release_commands support to release pipeline

## [0.33.9] - 2026-01-21

- Add context-aware component suggestions for version bump command

## [0.33.8] - 2026-01-21

- feat: Add project:subtarget colon syntax for CLI tools (both 'extra-chill:events' and 'extra-chill events' now work)

## [0.33.7] - 2026-01-21

- fix: is_workdir_clean() now correctly identifies clean repositories (fixes #6)

## [0.33.6] - 2026-01-21

### Added
- Add `component add-version-target` command for adding version targets without full JSON spec

### Changed
- Auto-insert `--` separator for trailing_var_arg commands (`component set`, `server set`, `test`) - intuitive syntax now works without explicit separator

## [0.33.5] - 2026-01-21

- Create engine/ directory with pipeline and executor extensions
- Move base_path.rs and slugify.rs to utils/

## [0.33.4] - 2026-01-21

- Remove ReleaseConfig - publish targets now derived purely from extensions with release.publish action

## [0.33.3] - 2026-01-21

- Fix publish step extension lookup by parsing prefix once in from_str (single source of truth)
- Add cleanup step to release pipeline to remove target/distrib/ after publish

## [0.33.2] - 2026-01-21

- **Release Pipeline**: Fixed architecture to use extension's `release.package` action for artifact creation instead of direct build

## [0.33.1] - 2026-01-21

- fix: add missing Build step to release pipeline

## [0.33.0] - 2026-01-21

- Refactor release system: built-in core steps (commit, tag, push) with config-driven publish targets

## [0.32.7] - 2026-01-21

- Fix release config-first: component release.steps now respected instead of overwritten with generated defaults
- Remove --no-tag, --no-push, --no-commit flags from release command (use git primitives for partial workflows)

## [0.32.6] - 2026-01-21

- Add --deploy flag to release command for automatic deployment to all projects using the component
- Add --force flag to deploy command to allow deployment with uncommitted changes
- Fix version commit detection to recognize 'Version X.Y.Z' and 'Version bump to X.Y.Z' commit formats

## [0.32.5] - 2026-01-20

- Add 'homeboy extension show' command for detailed extension inspection

## [0.32.4] - 2026-01-20

- Add build-time local_path validation with clear error messages
- Add tilde expansion (~/) support for component local_path
- Add gap_details to init output for inline config gap explanations
- Add project auto-detection for deploy when only component ID provided
- Add normalize_args() to handle both quoted and unquoted CLI tool arguments

## [0.32.3] - 2026-01-20

- Consolidate release runner, fix step ordering

## [0.32.2] - 2026-01-20

### Added
- Add validate_local_path with self-healing hints for misconfigured components

## [0.32.1] - 2026-01-20

### Refactored
- Refactor release extension into cleaner extension structure

## [0.32.0] - 2026-01-20

### Added
- Add `version bump` command as alias for release (e.g., `homeboy version bump homeboy minor`)
- Add `--no-commit` flag to release command to skip auto-committing uncommitted changes
- Add `--commit-message` flag to release command for custom pre-release commit messages
- Add version show shorthand: `homeboy version <component>` now works as `homeboy version show <component>`

### Changed
- Release command now auto-commits uncommitted changes by default (use `--no-commit` to opt-out)
- Improve build verification before release

## [0.31.1] - 2026-01-20

- Consolidate I/O primitives and option chains for cleaner code

## [0.31.0] - 2026-01-20

### Added
- Add release command flags: --dry-run (preview), --local (skip push/publish), --publish (force full pipeline), --no-tag, --no-push

### Changed
- Unify release command: 'homeboy release <component> <patch|minor|major>' now handles version bump, commit, tag, and optional push/publish in one flow

### Removed
- Remove 'version bump' command - use 'homeboy release <component> patch|minor|major' instead
- Remove 'release run' and 'release plan' subcommands - use 'homeboy release <component> patch|minor|major [--dry-run]' instead

## [0.30.16] - 2026-01-20

### Added
- Add --project/-p flag to deploy command for explicit project specification

### Refactored
- Add utils/io extension with read_file and write_file helpers for consistent error handling

### Refactored
- Add json_path_str helper for nested JSON value extraction

## [0.30.15] - 2026-01-20

### Added
- Add Refactored changelog entry type with Refactor alias
- Add stage_files function for targeted git staging operations
- Auto-stage changelog changes before version bump clean-tree check
- Add lines_to_vec helper for common string-to-vec-lines pattern

### Changed
- Replace manual error checking with validation helper utilities across codebase
- Use String::from instead of .to_string() for owned string conversions

### Fixed
- Improve orphaned tag auto-fix messaging in release pipeline

## [0.30.14] - 2026-01-20

### Changed
- Consolidate utils and create command primitives

### Fixed
- Fix changelog init --configure circular error
- Accept changelog_targets as alias for changelog_target

## [0.30.13] - 2026-01-20

- Auto-fix orphaned tags in git.tag step instead of failing with hints

## [0.30.12] - 2026-01-20

- Add pre_version_bump_commands for staging build artifacts before clean-tree check
- Improve orphaned tag hint with one-liner fix command
- Enhance version bump commit failure error with recovery guidance

## [0.30.11] - 2026-01-20

- Migrate changelog, init, and deploy to use parser utilities for version extraction and path resolution

## [0.30.10] - 2026-01-20

- Wire up version-aware baseline detection in changes() to fix stale tag mismatch
- Add unconfigured version pattern detection to init warnings
- Clarify init command help text and documentation

## [0.30.9] - 2026-01-20

### Added
- Added: Comprehensive schema, architecture, and developer guide documentation

## [0.30.8] - 2026-01-20

- Make release git.tag step idempotent to work with version bump tags
- Add release pipeline hint after version bump tagging

## [0.30.7] - 2026-01-20

### Changed
- Improve version bump error hints to explain why working tree must be clean

## [0.30.6] - 2026-01-20

### Added
- Require clean working tree before version bump with helpful hints

## [0.30.5] - 2026-01-20

### Added
- Add automatic git tag creation after version bump commits

## [0.30.4] - 2026-01-20

- Accept --json flag as no-op on commands that return JSON by default (init, test, lint, release, upgrade)

## [0.30.3] - 2026-01-20

- Add plural aliases for entity commands (servers, components, extensions)

## [0.30.2] - 2026-01-20

- Fixed: Version baseline detection now correctly identifies stale tags and falls back to release commits for accurate commit counts

## [0.30.1] - 2026-01-20

### Added
- Added: `status` alias for `init` command

### Removed
- Removed: `context` command (use `init` instead)

## [0.30.0] - 2026-01-19

- Added component auto-detection in `homeboy changes` - auto-uses detected component when exactly one matched
- Added version/baseline alignment warning in `homeboy init` when source file version differs from git baseline
- Renamed `GitSnapshot.version_baseline` to `baseline_ref` for consistency with `changes` output

## [0.29.3] - 2026-01-19

- Remove redundant fields from init JSON output (context.contained_components, context.components, context.command)
- Add gaps field to components array in init output for parent context
- Make version block conditional on managed context in init output
- Skip empty settings HashMap serialization in extension configs
- Skip null suggestion field serialization in context output

## [0.29.2] - 2026-01-19

- Add per-component release_state to init output (commits_since_version, has_uncommitted_changes, baseline_ref)

## [0.29.1] - 2026-01-19

- Add --status as visible alias for deploy --check

## [0.29.0] - 2026-01-19

- Add docs audit subcommand for link validation and staleness detection
- Change docs scaffold to require component_id for consistency with other commands
- Fix docs topic parsing to not consume flags as part of topic path
- Add agent_context_files to init output showing git-tracked markdown files

## [0.28.1] - 2026-01-19

- Add capability hints to lint and test commands for better discoverability

## [0.28.0] - 2026-01-19

- Add release state tracking to init and deploy --check for detecting unreleased work

## [0.27.13] - 2026-01-19

- Fix passthrough arguments documentation to be generic

## [0.27.12] - 2026-01-19

- Add shell quoting documentation to wp command docs
- Display subtargets in homeboy init output for project discoverability
- Support both argument orders for deploy command (project-first or component-first)
- Add CLI tool suggestions to homeboy init next_steps when extensions have CLI tools

## [0.27.11] - 2026-01-19

### Added
- Added lint summary header showing error/warning counts at top of output
- Added --sniffs, --exclude-sniffs, and --category flags for lint filtering

### Changed
- Enhanced --summary to show top violations by sniff type

### Fixed
- Fixed custom fixers ignoring --file and --glob targets

## [0.27.10] - 2026-01-19

### Added
- Add --level flag as alternative to positional bump type in version bump command

### Fixed
- Make --changed-only flag language-agnostic (removes hardcoded .php filter)

## [0.27.9] - 2026-01-19

### Added
- Add --changed-only flag to lint command for focusing on modified PHP files
- Add prerequisites validation to release plan (warns about empty changelog)

## [0.27.8] - 2026-01-19

### Fixed
- Pass HOMEBOY_MODULE_PATH environment variable to build commands

## [0.27.7] - 2026-01-19

### Fixed
- Fixed: version set no longer validates/finalizes changelog (version-only operation)
- Fixed: version show now displays all configured version targets, not just the primary

## [0.27.6] - 2026-01-19

- Fixed: settings_flags now applied during direct execution for local CLI tools

## [0.27.5] - 2026-01-19

### Added
- Add ExtensionRunner builder for unified test/lint script orchestration
- Add ReleaseStepType enum for typed release pipeline steps

### Changed
- Refactor lint and test commands to use ExtensionRunner, reducing code duplication
- Simplify deploy, version, and SSH commands with shared utilities

## [0.27.4] - 2026-01-18

### Added
- Immediate 'homeboy is working...' feedback for TTY sessions

## [0.27.3] - 2026-01-18

### Security
- Fix heredoc injection vulnerability in file write operations
- Fix infinite loop in pattern replacement when pattern appears in replacement
- Fix grep failing on single files (was always using recursive flag)
- Fix non-portable --max-depth in grep (now uses find|xargs)
- Fix race condition in file prepend operations (now uses mktemp)
- Fix inconsistent echo behavior in append/prepend (now uses printf)

### Added
- Add --raw flag to `file read` for output without JSON wrapper

### Changed
- Separate stdout/stderr in lint and test command output

## [0.27.2] - 2026-01-18

- Add granular lint options: --file, --glob, and --errors-only flags for targeted linting

## [0.27.1] - 2026-01-18

- Add --summary flag to lint command for compact output

## [0.27.0] - 2026-01-18

- feat: make build_artifact optional—extensions can provide artifact_pattern for automatic resolution
- feat: deploy command supports --project flag as alternative to positional argument
- feat: context gaps now detect missing buildArtifact when remotePath is configured
- fix: version parsing now trims content for VERSION files with trailing newlines
- docs: comprehensive README overhaul with workflow examples and extension system documentation

## [0.26.7] - 2026-01-18

- Add `homeboy lint` command for standalone code linting via extension scripts
- Add `--skip-lint` flag to `homeboy test` to run tests without linting
- Add `pre_build_script` hook to extension BuildConfig for pre-build validation

## [0.26.6] - 2026-01-18

### Added
- NullableUpdate<T> type alias for three-state update semantics in CLI commands

### Changed
- refactor extension.rs into extension/ directory with focused submodules (manifest, execution, scope, lifecycle, exec_context)
- replace .unwrap() calls with .expect() for safer error handling across codebase
- extract duplicate template variable building into DbContext::base_template_vars()
- unify scp_file and scp_recursive into shared scp_transfer() function
- use OnceLock for lazy regex compilation in template resolution

### Fixed
- load_all_modules() calls now use unwrap_or_default() to handle errors gracefully

## [0.26.5] - 2026-01-18

- feat: add --stream and --no-stream flags to extension run command for explicit output control
- feat: add HOMEBOY_COMPONENT_PATH environment variable to test runners
- feat: make ExtensionExecutionMode enum public for extension integration

## [0.26.4] - 2026-01-18

- feat: new test command for running component test suites with extension-based infrastructure

## [0.26.3] - 2026-01-18

- feat: enhanced extension list JSON output with CLI tool info, available actions, and runtime status flags
- feat: added context-aware error hints suggesting 'homeboy init' when project context is missing

## [0.26.2] - 2026-01-18

- Test dry-run validation

## [0.26.1] - 2026-01-18

### Fixed
- version bump command now accepts bump type as positional argument without requiring -- separator

## [0.26.0] - 2026-01-18

### Added
- Added: automatic docs topic resolution with fallback prefixes for common shortcuts (e.g., 'version' → 'commands/version', 'generation' → 'documentation/generation')

### Changed
- Changed: config directory moved to universal ~/.config/homeboy/ on all platforms (previously ~/Library/Application Support/homeboy on macOS). Users may need to migrate config files manually.

## [0.25.4] - 2026-01-18

- Fixed: changelog init now checks for existing changelog files before creating new ones, preventing duplicates

## [0.25.1] - 2026-01-17

- Enforce changelog hygiene: version set/bump require clean changelog, release rejects unreleased content

## [0.25.0] - 2026-01-17

### Fixed
- Require explicit subtarget when project has subtargets configured, preventing unintended main site operations in multisite networks

## [0.24.3] - 2026-01-17

- feat: homeboy version show defaults to binary version when no component_id provided

## [0.24.2] - 2026-01-17

- fix: upgrade restart command now uses --version instead of version show to avoid component_id error

## [0.24.1] - 2026-01-17

- fix: Improve error message when `homeboy changes` runs without component ID

## [0.24.0] - 2026-01-17

- feat: Add extension-provided build script support with priority-based command resolution

## [0.23.0] - 2026-01-16

- feat: Add settings_flags to CLI extensions for automatic flag injection from project settings

## [0.22.10] - 2026-01-16

- fix: Release pipeline always creates annotated tags ensuring git push --follow-tags works correctly

## [0.22.9] - 2026-01-16

### Fixed
- Release pipeline amends previous release commit instead of creating duplicates

## [0.22.8] - 2026-01-16

- fix: release pipeline pushes commits with tags and skips duplicate commits

## [0.22.7] - 2026-01-16

- Make path optional in logs show - shows all pinned logs when omitted

## [0.22.6] - 2026-01-16

- Add changelog show subcommand with optional component_id support

## [0.22.5] - 2026-01-16

- Allow `homeboy release <component>` as shorthand for `homeboy release run <component>`

## [0.22.4] - 2026-01-16

- Support --patch/--minor/--major flag syntax for version bump command

## [0.22.3] - 2026-01-16

### Added
- Add --type flag to changelog add command for Keep a Changelog subsection placement

### Fixed
- Improve deploy error message when component ID provided instead of project ID

## [0.22.2] - 2026-01-16

- Add --changelog-target flag to component create command
- Make build_artifact and remote_path optional in component create for library projects
- Improve git.tag error handling with contextual hints for tag conflicts

## [0.22.1] - 2026-01-16

- Update documentation to remove all --cwd references

## [0.22.0] - 2026-01-16

- **BREAKING**: Remove `--cwd` flag entirely from CLI - component IDs are THE way to use Homeboy (decouples commands from directory location)
- **BREAKING**: `version bump` now auto-commits version changes. Use `--no-commit` to opt out.
- Add `--dry-run` flag to `version bump` for simulating version changes
- Add changelog warning when Next section is empty during version bump
- Add template variable syntax support for both `{var}` and `{{var}}` in extract commands
- Add deploy override visibility in dry-run mode with "Would..." messaging
- Create unified template variables reference documentation

## [0.21.0] - 2026-01-16

- Add generic extension-based deploy override system for platform-specific install commands
- Add `heck` crate for automatic camelCase/snake_case key normalization in config merges
- Fix SIGPIPE panic when piping CLI output to commands like `head`
- Fix `success: true` missing from component set single-item responses
- Fix deploy error messages to include exit code and fall back to stdout when stderr is empty

## [0.20.9] - 2026-01-15

- Omit empty Unreleased section when finalizing releases

## [0.20.8] - 2026-01-15

- Add init snapshots for version, git status, last release, and changelog preview
- Surface extension readiness details with failure reason and output
- Omit empty Unreleased section when finalizing releases

## [0.20.7] - 2026-01-15

- Add -m flag for changelog add command (consistent with git commit/tag)
- Support bulk changelog entries via repeatable -m flags
- Add git.tag and git.push steps to release pipeline

## [0.20.6] - 2026-01-15

- add init next_steps guidance for agents

## [0.20.5] - 2026-01-15

- Add git.commit as core release step (auto-inserted before git.tag)
- Add pre-flight validation to fail early on uncommitted changes
- Add PartialSuccess pipeline status with summary output
- Remove GitHub Actions release workflow (replaced by local system)

## [0.20.4] - 2026-01-15

- Add release workflow guidance across docs and README
- Expose database template vars for db CLI commands

## [0.20.3] - 2026-01-15

- **Release system now fully replaces GitHub Actions** - Complete local release pipeline with package, GitHub release, Homebrew tap, and crates.io publishing
- Fix extension template variable to use snake_case convention (`extension_path`)
- Fix macOS bash 3.x compatibility in extension publish scripts (replace `readarray` with POSIX `while read`)
- Add `dist-manifest.json` to .gitignore for cleaner working directory

## [0.20.2] - 2026-01-15

- Prepare release pipeline for extension-driven publishing

## [0.20.1] - 2026-01-15

- Fix release pipeline executor and extension action runtime

## [0.20.0] - 2026-01-15

- Add parallel pipeline planner/executor for releases
- Add component-scoped release planner and runner
- Support extension actions for release payloads and command execution
- Add extension-driven release payload context (version/tag/notes/artifacts)
- Add git include/exclude file scoping
- Add config replace option for set commands
- Improve changelog CLI help and detection

## [0.19.3] - 2026-01-15

- Remove agent-instructions directory - docs are the single source of truth
- Simplify build.rs to only embed docs/
- Update README with streamlined agent setup instructions

## [0.19.2] - 2026-01-15

- Add post_version_bump_commands hook to run commands after version bumps
- Run cargo publish with --locked to prevent lockfile drift in releases

## [0.19.1] - 2026-01-15

- fix: `homeboy changes` surfaces noisy untracked hints and respects `.gitignore`

## [0.19.0] - 2026-01-15

- feat: add `homeboy config` command for global configuration
- feat: configurable SCP flags, permissions, version detection patterns
- feat: configurable install method detection and upgrade commands
- fix: `homeboy docs` uses raw markdown output only, removes --list flag

## [0.18.0] - 2026-01-15

- Add belt & suspenders permission fixing (before build + after extraction)
- Add -O flag for SCP legacy protocol compatibility (OpenSSH 9.x)
- Add verbose output for deploy steps (mkdir/upload/extract)
- Add SSH auto-cd to project base_path when project is resolved
- Fix changelog finalization error propagation with helpful hints
- Inherit changelog settings from project when component has single project

## 0.17.0

- Agnostic local/remote command execution - db, logs, files now work for local projects
- Init command returns structured JSON with context, servers, projects, components, and extensions
- New executor.rs provides unified command routing based on project config
- Renamed remote_files extension to files (environment-agnostic)

## 0.16.0

- **BREAKING**: JSON output now uses native snake_case field names (e.g., project_id, server_id, base_path)
- Remove all serde camelCase conversion annotations
- Consolidate json extension into config and output extensions

## 0.15.0

- Added bulk merge support for component/project/server set commands
- Improved coding-agent UX: auto-detect commit message vs JSON, better fuzzy matching, and fixed --cwd parsing
- Refactored create flow into a single unified function
- Removed dry-run mode and related behavior
- Improved auto-detection tests
- Included pending context and documentation changes

## 0.14.0

- Merge workspace into single crate for crates.io publishing
- Add src/core/ architectural boundary separating library from CLI
- Library users get ergonomic imports via re-exports (homeboy::config instead of homeboy::core::config)

## 0.13.0

- Add --staged-only flag to git commit for committing only pre-staged changes
- Add --files flag to git commit for staging and committing specific files
- Add commit_from_json() for unified JSON input with auto-detect single vs bulk format
- Align git commit JSON input pattern with component set (positional spec, stdin, @file support)

## 0.12.0

- Add `homeboy upgrade` command for self-updates
- Improve `homeboy context` output for monorepo roots (show contained components)
- Fix `homeboy changes` single-target JSON output envelope
- Clarify recommended release workflow in docs

## 0.11.0

- Add universal fuzzy matching for entity not-found errors
- Align changes output examples with implementation

## 0.10.0

- Refactor ID resolution and standardize resolving IDs from directory names
- Add `homeboy extension set` to merge extension manifest JSON
- Centralize config entity rename logic
- Refactor project pin/unpin API with unified options

## 0.9.0

- Add remote find and grep commands for server file search
- Add helpful hints to not-found error messages
- Refactor git extension for cleaner baseline detection
- Add slugify extension
- Documentation updates across commands

## 0.8.0

- Refactor JSON output envelope (remove warnings payload; simplify command JSON mapping)
- Unify bulk command outputs under BulkResult/ItemOutcome with success/failure summaries
- Remove per-project extension enablement checks; use global extension manifests for build/deploy/db/version defaults
- Deploy output: rename components -> results and add total to summary

## 0.7.5

- Fix Homebrew formula name: cargo-dist now generates homeboy.rb instead of homeboy-cli.rb

## 0.7.4

- Update skill documentation with changelog ops, version set, and bulk JSON syntax
- Support positional component filtering in changes command

## 0.7.3

- Support positional message argument for changelog add and git commit commands
- Add version set command for direct version assignment

## 0.7.2

- Add tiered fallback for changes command when no tags exist (version commits → last 10 commits)

## 0.7.1

- Align homeboy init docs source with agent-instructions
- Simplify changelog add --json format to match other bulk commands

## 0.7.0

- Refactor CLI commands to delegate business logic to the core library
- Add core git extension for component-scoped git operations
- Add core version extension for version target read/update utilities
- Improve changes command output for local working tree state
- Refresh embedded CLI docs and JSON output contract

## 0.6.0

- Add universal --merge flag for component/project/server set commands
- Fix changelog entry spacing to preserve blank line before next version
- Refactor core into a headless/public API; treat the CLI as one interface
- Move business logic into the `homeboy` core library and reduce CLI responsibilities
- Standardize command/output layers and keep TTY concerns in the CLI
- Introduce/expand the extension system and extension settings
- Add generic auth support plus a generic API client/command
- Remove/adjust doctor and error commands during stabilization

## 0.5.0

- Refactor deploy to use a generic core implementation
- Replace component isNetwork flag with extractCommand for post-upload extraction
- Unify extension runtime config around runCommand/setupCommand/readyCheck/env and remove plugin-specific fields
- Update docs and examples for new generic deployment and extension behavior

## 0.4.1

- Rename plugin terminology to extension across CLI/docs
- Remove active project concept; require explicit --project where needed
- Update extension manifest filename to `<extension_id>.json`

## 0.4.0

- Unify plugins and extensions under a single extension manifest and config surface
- Remove plugin command and plugin manifest subsystem; migrate CLI/db/deploy/version/build to extension-based lookups
- Rename config fields: plugins→extensions, plugin_settings→extension_settings, extensions→scoped_modules (superseded by extensions field in current releases)

## 0.3.0

- Add plugin support (nodejs/wordpress)
- Add plugin command and plugin manifest integration
- Improve deploy/build/version command behavior and outputs

## 0.2.19

- Fix inverted version validation condition to prevent gaps instead of blocking valid bumps

## 0.2.18

- Fix shell argument escaping for wp and pm2 commands with special characters
- Centralize shell escaping in shell.rs extension with quote_arg, quote_args, quote_path functions
- Fix unescaped file paths in logs and file commands
- Remove redundant escaping functions from template.rs, ssh/client.rs, and deploy.rs

## 0.2.17

- Add project set --component-ids to replace component attachments
- Add project components add/remove/clear subcommands
- Add tests for project component attachment workflows

## 0.2.15

- Derive git tag name
- Internal refactor

## 0.2.14

- Fix unused imports warnings

## 0.2.13

- Project rewrite
- Internal cleanup

## 0.2.12

- Refactor command implementations to reduce boilerplate
- Add new CLI flags support
- Fix changelog formatting

## 0.2.10

- Clean up version show JSON output

## 0.2.9

- Fix clippy warnings (argument bundling, test extension ordering)

## 0.2.8

- docs: homeboy docs outputs raw markdown by default
- changelog: homeboy changelog outputs raw markdown (removed show subcommand)

## 0.2.7

- Default JSON output envelope; allow interactive passthrough
- Require stdin+stdout TTY for interactive passthrough commands
- Standardize `--json` input spec handling for subcommands that support it (`project create --json`, `changelog --json`)
- Fix changelog finalization formatting

## 0.2.5

- added overlooked config command back in
- docs updated
- extension standardized data contract

## 0.2.4

- Restore 'homeboy config' command wiring
- Update command docs to include config

## 0.2.3

- Fix changelog finalize placing ## Unreleased at top instead of between versions
- Fix changelog item insertion removing extra blank lines between items

## 0.2.2

- Add scan_json_dir<T>() helper to json extension for directory scanning
- Refactor config list functions to use centralized json helpers
- Refactor extension loading to use read_json_file_typed()
- Internal refactor

## 0.2.1

- Default app config values are serialized (no more Option-based defaults for DB settings)
- DB commands now read default CLI path/host/port from AppConfig instead of resolve helpers

## 0.2.0

### Improvements
- **Config schema**: Introduce `homeboy config` command group + `ConfigKeys` schema listing to standardize how config keys are described/exposed.
- **Config records**: Standardize config identity via `slugify_id()` + `SlugIdentifiable::slug_id()` and enforce id/name consistency in `ConfigManager::save_server()` and `ConfigManager::save_component()`.
- **App config**: Extend `AppConfig` with `installedModules: HashMap<String, InstalledModuleConfig>`; each extension stores `settings: HashMap<String, Value>` and optional `sourceUrl` (stored in the extension manifest).
- **Extension scoping**: Add `ExtensionScope::{effective_settings, validate_project_compatibility, resolve_component_scope}` to merge settings across app/project/component and validate `ExtensionManifest.requires` (for example: `components`).
- **Extension execution**: Tighten `homeboy extension run` to require an installed/configured entry and resolve project/component context when CLI templates reference project variables.
- **Command context**: Refactor SSH/base-path resolution to shared context helpers (used by `db`/`deploy`) for more consistent configuration errors.
- **Docs**: Normalize docs placeholders (`<project_id>`, `<server_id>`, `<component_id>`) across embedded CLI documentation.

## 0.1.13

### Improvements
- **Changelog**: `homeboy changelog add` auto-detects changelog path when `changelogTargets` is not configured.
- **Changelog**: Default next section label is `Unreleased` (aliases include `[Unreleased]`).
- **Version**: `homeboy version bump` finalizes the "next" section into the new version section whenever `--changelog-add` is used.

## 0.1.12

### Improvements
- **Changelog**: Promote `homeboy changelog` from a shortcut to a subcommand group with `show` and `add`.
- **Changelog**: Add `homeboy changelog add <component_id> <message>` to append items to the “next” section (defaults to `Unreleased`).
- **Changelog**: Auto-detect changelog path (`CHANGELOG.md` or `docs/changelog.md`) when `changelogTargets` is not configured.
- **Config**: Support `changelogTargets` + `changelogNextSectionLabel`/`changelogNextSectionAliases` at component/project/app levels.
- **Version**: Write JSON version bumps via the `version` key (pretty-printed) when using the default JSON version pattern.
- **Deploy**: Load components via `ConfigManager` instead of ad-hoc JSON parsing.

## 0.1.11

### Improvements
- **Docs**: Expanded `docs/index.md` to include configuration/state directory layout and a clearer documentation index.
- **Docs/Positioning**: Refined README messaging to emphasize Homeboy’s LLM-first focus.

## 0.1.10

### Improvements
- **Extensions**: Added git-based extension workflows: `homeboy extension install`, `homeboy extension update`, and `homeboy extension uninstall`.
- **Extensions**: Added `.install.json` metadata (stored inside each extension directory) to enable reliable updates from the original source.
- **Docs/Positioning**: Updated README and docs index to reflect LLM-first focus and Homeboy data directory layout.

## 0.1.9

### Improvements
- **Project management**: Added `homeboy project list` and `homeboy project pin` subcommands to manage pinned files/logs per project.
- **Config correctness**: Project configs are a strict `ProjectRecord` (`id` derived via `slugify_id(name)`) with validation to prevent mismatched IDs and to clear `active_project_id` when a project is deleted.
- **Docs**: Updated embedded docs to reflect new/removed commands.

## 0.1.8

### Improvements
- **Versioning**: `versionTargets` are now first-class for component version management (supports multiple files and multiple matches per file, with strict validation).
- **Deploy**: Reads the component version from `versionTargets[0]` for local/remote comparisons.

## 0.1.7

### Improvements
- **Component configuration**: Support `versionTargets` (multiple version targets) and optional `buildCommand` in component config.
- **Version bumping**: `homeboy version bump` validates that all matches in each target are the same version before replacing.
- **Deploy JSON output**: Deploy results include `artifactPath`, `remotePath`, `buildCommand`, `buildExitCode`, and an upload exit code for clearer automation.
- **Docs refresh**: Updated command docs + JSON output contract; removed outdated command/contract doc.

## 0.1.6

### New Features
- **Embedded docs**: Embed `homeboy/docs/**/*.md` into the CLI binary at build time, so `homeboy docs` works in Homebrew/releases.
- **Docs source of truth**: Keep CLI documentation under `docs/` and embed it into the CLI binary.

- **Docs topic listing**: `available_topics` is now generated dynamically from embedded keys (newline-separated).

## 0.1.5

### Breaking Changes
- **Docs Command Output**: `homeboy docs` now prints embedded markdown to stdout by default (instead of paging).

### New Features
- **Core Path Utilities**: Added `homeboy_core::base_path` helpers for base path validation and remote path joining (`join_remote_path`, `join_remote_child`, `remote_dirname`).
- **Core Shell Utilities**: Added `homeboy_core::shell::cd_and()` to build safe "cd && <cmd>" strings.
- **Core Token Utilities**: Added `homeboy_core::token` helpers for case-insensitive identifiers and doc topic normalization.

### Improvements
- **Unified JSON Output**: CLI commands now return typed structs and are serialized in `crates/homeboy/src/main.rs`, standardizing success/error output and exit codes.
- **Docs & Skill Updates**: Updated documentation and the Homeboy skill.

## 0.1.4

### New Features
- **Build Command**: New `homeboy build <component>` for component-scoped builds
  - Runs a component build in its `local_path`

### Improvements
- **Version Utilities**: Refactored version parsing to shared `homeboy` core library
  - `parse_version`, `default_pattern_for_file`, `increment_version` now in core
  - Enables future reuse across CLI components

## 0.1.3

### New Features
- **Version Command**: New `homeboy version` command for component-scoped version management
  - `show` - Display current version from component's version_file
  - `bump` - Increment version (patch/minor/major) and write back to file
  - Auto-detects patterns for .toml, .json, .php files

## 0.1.2

### New Features
- **Git Command**: New `homeboy git` command for component-scoped git operations
  - `status` - Show git status for a component
  - `commit` - Stage all changes and commit with message
  - `push` - Push local commits to remote (with `--tags` flag support)
  - `pull` - Pull remote changes
  - `tag` - Create git tags (lightweight or annotated with `-m`)

### Improvements
- **Dogfooding Support**: Homeboy can now manage its own releases via git commands

## 0.1.1

### Breaking Changes
- **Config Rename**: `local_cli` renamed to `local_environment` in project configuration JSON files.

### Improvements
- **Deploy Command**: Improved deployment workflow.
- **Extension Command**: Enhanced CLI extension execution with better variable substitution.
- **PM2 Command**: Improved PM2 command handling for Node.js projects.
- **WP Command**: Improved WP-CLI command handling for WordPress projects.

## 0.1.0

Initial release.
- Project, server, and component management
- Remote SSH operations (wp, pm2, ssh, db, file, logs)
- Deploy and pin commands
- CLI extension execution
- Shared configuration across clients
