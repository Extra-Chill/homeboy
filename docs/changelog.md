# Changelog

All notable changes to Homeboy CLI are documented in this file.

(This file is embedded into the CLI binary and is also viewable via `homeboy changelog`.)

## [0.253.1] - 2026-06-22

### Changed
- Remove hidden lab command surface
- stabilize lab runner loop semantics expectations
- apply rustfmt after loop semantics rebase
- format lab contract descriptor assignment
- refactor(agent-task): split provider module
- centralize workload construction
- refactor(agent-task): reserve loop for durable controllers
- Unify Lab secret env handoff
- Add visible command manifest surface
- declare mutation patch capture
- Type agent-task discovery command args
- Remove deprecated file upload command

### Fixed
- remove playground/codebox ecosystem leaks from failure detection
- drop stale workload label helpers
- score preferred runners by readiness
- unify offloaded run evidence identity

## [0.253.0] - 2026-06-22

### Added
- feat(agent-task): concise Codebox provider preflight diagnostics (#5561)

## [0.252.1] - 2026-06-22

### Changed
- split github.rs god-file into modules (#5891)
- thin command adapters, move logic to core (#5902)
- extract shared field groups (#5899)
- Sync Lab runtime components from providers

### Fixed
- Fix changed-file clippy findings
- Fix Lab runtime component lint

## [0.252.0] - 2026-06-22

### Added
- feat(agent-task): surface backend selection and source before dispatch
- feat(agent-task): consistent discovery + list pagination

### Changed
- de-hardcode ecosystem literals (#5897)
- consolidate parallel git probe impls (#5901)
- Guard release tags behind default branch
- Add fuzz-safe lifecycle contracts
- cheapen homeboy quality gates
- Classify agent-task promote safety metadata
- Export fuzz manifest config
- simplify observation output runtime
- Unify Lab route contract policy
- Improve command safety manifest metadata
- Refine agent task runtime contracts
- Clarify generic fuzz campaign contract

### Fixed
- remove dead code in worktree (#5898)
- fail closed on off-branch release tags
- Fix lifecycle contract lint failures
- Fix fuzz sidecar manifest expectation
- repair observation runtime rebase
- prepare runner source checkout
- Fix Lab extension sync capability preflight
- Fix runtime status normalization borrow
- Fix runner lifecycle handoff contracts

## [0.251.0] - 2026-06-22

### Added
- feat(agent-task): reconcile/flag stale active Lab runs
- feat(agent-task): report secret-source readiness in auth status

### Changed
- Add fuzz operation family taxonomy
- Add fuzz coverage skip summaries
- Materialize Lab @file inputs

### Fixed
- fix(agent-task): carry dispatch component contracts
- keep artifact roots runner-local

## [0.250.4] - 2026-06-22

### Changed
- move worktree process execution into core service (#5879)
- move ecosystem literals out of dead_code detector (#5876)

## [0.250.3] - 2026-06-22

### Changed
- dedupe branch_slug into core (#5867)
- de-hardcode ecosystem validate/format fallbacks (#2240)

## [0.250.2] - 2026-06-22

### Fixed
- remove dead code mark_failed in phase_timing (#5863)
- unify parallel implementations (#5849)
- resolve correctness findings in deploy/release (#5852)

## [0.250.1] - 2026-06-22

### Changed
- Pass rig fuzz workload invocation context
- Apply Enterprise proxy to release gh calls
- Add fuzz replay metadata contract
- Improve Lab registry drift diagnostics
- Fail release packages missing frontend assets
- Validate file dependencies before release package
- Reuse release assets during deploy
- Ignore generated release artifacts in deploy gate
- Add CI failure triage digest
- Add fuzz inventory artifact contract
- Add dependent PR refresh helper
- Explain PR merge readiness states
- Add CI failure log triage helper
- Improve PR CI status next actions
- Support generated controller materialize specs
- Add ordered PR landing rebase plans
- Make changed-since Lab routing use git scope
- Persist fuzz run evidence records
- Add PR landing train command
- Add baseline-red differential CI gate
- Add PR CI readiness triage summary
- Refine PR landing blocker summary
- Add PR fleet landing helper
- Surface rustfmt files in lint failures
- Add PR mergeability reconcile command
- Handle baseline-red differential CI results
- Write structured lint test failure sidecars
- Apply current Rust formatting
- Add generic deterministic loop primitives

### Fixed
- resolve dead-code in phase_timing (#5844)
- Fix Lab offload contract borrowing
- Fix controller reconciliation import
- actually replace active binary on --method source and fail loudly if swap fails (#5772)
- Fix controller proof spec validation
- Fix agent task prompt command results
- Fix deterministic loop checkpoint borrows

## [0.250.0] - 2026-06-22

### Added
- require declared proof artifacts
- support manifest-only fuzz capability
- extend fuzz data contracts
- expose lifecycle contracts
- add GitHub Actions scope resolver in core (#2915)
- secure reverse runner broker authentication and pairing (#2990)
- feat(agent-task): add core deterministic loop primitive
- add core CI runner orchestration (slice, #2913)
- feat(agent-task): expose public fanout primitive
- execute extension fuzz runners
- support rig-declared workloads
- own the CI autofix transaction in core (#2916)
- add generic fuzz command skeleton
- record materialization proof metadata
- summarize fuzz coverage artifacts
- feat(agent-task): bind fanout child runs to artifacts
- add generic outcome threshold presets
- add product-neutral fuzz contracts

### Changed
- Make changed-since Lab offload use precomputed scope
- Reconcile stale runner jobs after restart
- Add PR landing triage dashboard
- Add queue-friendly worktree fanout helper
- Document architecture cleanup boundaries
- Refine runner workspace artifact contracts
- Add fuzz coverage completeness gates
- Add agent task prompt store
- Add runner workload contract metadata
- Add fuzz primitive contract surfaces
- Format Rust sources
- Add review audit profile override
- Prefer runner evidence lineage over timestamps
- route fleet metadata through command adapter
- Fail release planning when remote tag already exists
- Restore checkout after failed releases
- Fail project deploy readiness on missing component paths
- Refactor agent task Lab handoff parsing
- Make runner active-job status explicit
- Prune duplicate CLI surfaces
- Centralize GitHub CLI client handling
- Guard scoped checks from self-check fallback
- remove codebox-flavored provider fixtures
- Add fuzz replay skeleton
- neutralize product-specific fixtures
- refactor(agent-task): catalog provider discovery
- preserve reverse worker result artifacts
- centralize command descriptor metadata
- test(agent-task): align lab contract regression
- centralize supported contract labels
- skip redundant release quality work
- hide lab shortcut from advertised surface
- clear remaining structural findings (#5645)
- split god-files to clear structural findings (partial, #5645)
- Add controller dispatch provider config override
- migrate whole-tree detectors onto shared snapshots
- make test coverage and vacuity detectors language-agnostic (#2841)
- finish artifact releases through homeboy action

### Fixed
- unify parallel implementations to clear findings (#5711)
- resolve dead-code findings outside fleet files (#5665)
- remove core agnostic-source leaks (partial, #5664)
- Fix runner active job imports
- Fix deploy default tagged source planning
- require minor for release-train patch ranges
- populate detached exec handoff fields
- fix file edit dry-run contract
- restore main test compile
- fix(agent-task): update providers test args
- fix(agent-task): export catalog dispatch helpers
- repair compile drift after main updates
- repair rebased runner compile drift
- align exec fixtures with handoff options
- restore PR compile compatibility
- resolve compiler/dead-code findings
- DRY duplication and near-duplication findings
- clone exec artifacts before job move
- avoid stored failure context field
- pass results file contract to runners
- satisfy contract label lint
- surface Lab offload failure context
- ingest runner results file
- fix(agent-task): signal live run process trees
- fix(agent-task): guard lab-only fanout handoff
- fix(agent-task): honor loop git workspace contracts
- fix(agent-task): preflight loop verify gates
- fix(agent-task): fail fast on runner backend readiness
- fix(agent-task): route queued lab runs on runner
- fix(agent-task): reject empty required typed artifacts
- fix(agent-task): restore main CI after loop fanout
- centralize artifact viewer descriptors
- surface generic coverage proof cues
- Fix Lab changed-scope quality offload
- resolve thin-adapter and field-pattern findings (#5647, #5642)
- support lab runner routing
- pass path for ad-hoc verification
- strengthen vacuous tests to clear test_quality findings (partial, #5646)
- resolve failing test (#5650)
- fix(agent-task): update scheduler fuzz artifact fixture
- fix(agent-task): update fanout artifact fixture
- keep findings identical after shared-snapshot migration
- update self-check lint args

## [0.249.0] - 2026-06-21

### Added
- add explicit refresh/setup mode for CI to call core instead of hardcoding (#2917)
- feat(agent-task): surface controller acceptance gates
- feat(agent-task): add generic artifact roles
- feat(agent-task): add controller from-spec doctor
- summarize synced workspace source
- feat(agent-task): summarize controller runs

### Changed
- scope changed-scope detectors to changed files to avoid timeout (#3609)

### Fixed
- fix(agent-task): improve controller resume commands
- fix(agent-task): clarify provider id selector
- classify differential failure origins

## [0.248.1] - 2026-06-21

### Fixed
- update self-check lint args

## [0.248.0] - 2026-06-21

### Added
- highlight key artifacts in summaries

### Changed
- clear remaining structural finding (#5577)
- split god-files and high-item-count files to clear structural findings (partial, #5577)

### Fixed
- fix(agent-task): report provider selector mismatches
- clear remaining field-pattern and intra-method-dup findings (#5576, #5583)
- repair tests broken by structural module splits (#5599)
- summarize regression threshold metadata
- fix(agent-task): validate command envelope proofs
- surface bench prepare failure details
- remove dead code findings (#5581)
- fix(agent-task): summarize controller resume failures

## [0.247.6] - 2026-06-21

### Changed
- extract repeated field groups into shared structs (#5576)
- Mark failed bench hotspots in run summaries
- refactor(agent-task): delegate controller execution policy to reusable core service (#4303)

### Fixed
- remove core agnostic-source boundary leaks (#5580)
- capture version verifier output
- resolve test-phase failure (#5578)
- accept materialized controller gates
- fix(agent-task): materialize symlinked plan dependencies for Lab offload (#3913)
- resolve intermittent 502s for parallel static asset requests (#4062)

## [0.247.5] - 2026-06-21

### Changed
- resolve structural findings (#5558)
- extract agent-task provider preflight from lab offload god-module to harden API boundary (#4303)

### Fixed
- fix(agent-runtime): prefer extension runtimes over stale cache
- fix(agent-task): add generic provider preflight diagnostics
- validate runtime provider discovery on install
- fetch direct artifacts as bytes
- fail stale runner upgrade reports

## [0.247.4] - 2026-06-21

### Fixed
- resolve failing test (#5559)

## [0.247.3] - 2026-06-20

### Fixed
- fix(agent-task): fail missing typed artifact outcomes

## [0.247.2] - 2026-06-20

### Changed
- Surface bench coverage summaries

### Fixed
- Fix bench coverage summary checks

## [0.247.1] - 2026-06-20

### Fixed
- DRY intra-method duplication (#5546)

## [0.247.0] - 2026-06-20

### Added
- persist trace compare pair artifacts as first-class evidence (#4233)
- support non-blocking preview-consumer runs for held preview URLs (#4485)
- add post-deploy front-end smoke check to catch runtime-fataling releases (#5471)

### Changed
- resolve structural findings (#5547)
- resolve structural finding in bench/parsing (#5517)
- Apply bench hotspot formatting
- Avoid command helper type classification
- Align bench hotspot helper names
- Keep bench hotspot fixtures generic
- Surface bench hotspots in summaries

### Fixed
- read artifact-backed metrics in browser evidence compare (#4621)
- handle macOS dark-wake keychain failure in secret resolution (#4162)
- align homeboy file root with deploy plugin paths (#5456)
- resolve failing test (#5536)

## [0.246.1] - 2026-06-20

### Changed
- Carry controller workflow artifacts forward
- split god-files to clear structural findings (#5492)

### Fixed
- make release commit+tag concurrency-safe against base-branch races (#5502)
- narrow shared scripts lifecycle install after helper migration (#4907)
- remove core agnostic-source boundary leaks (#5504)
- resolve main lint break (rustfmt violation in bench parsing test)
- remove domain term from bench fixture
- remove core agnostic-source boundary leaks (#5490)

## [0.246.0] - 2026-06-20

### Added
- add generic phase-level timing spans for hot commands (#3002)
- auto-merge generated baseline-only homeboy.json conflicts (#3518)
- feat(agent-task): add one-command cook readiness repair (#4864)
- expose authoritative binary/runtime view across controller and runners (#4861)
- add artifact get command
- tag output variant
- require apply for exec
- enrich lifecycle inspection
- add phase metadata
- tag output variants
- expose retrieval capability
- expose reverse artifact metadata
- require apply for risky modes
- guard mutating api requests
- add reverse claim reconciliation route
- deepen command manifest metadata
- interrupt reverse jobs on cancel
- require apply for destructive commands
- expose safety manifest json
- mirror reverse broker evidence
- propagate reverse cancellation
- add command safety manifest
- add thin-command-adapter enforcement detector (#4384)
- compact default output + surface artifacts in run summaries (#3257, #3260)
- surface reverse broker jobs
- add lifecycle for bundled/retired components
- feat(agent-task): surface codebox provider failure diagnostics and artifact refs

### Changed
- Allow source upgrades from detached checkouts
- Use source checkout identity for runner upgrades
- Apply rustfmt to pnpm provider fix
- Use pnpm workspace roots for dependency installs
- Preserve invalid bench artifact paths for validation
- Require git Lab workspace for explicit controller backends
- Hide internal commands from public surface checks
- Require git Lab workspace for patch controller dispatch
- Apply rustfmt after main merge
- Ignore inline bench artifact payloads
- Preserve explicit bench failure classifications
- consolidate command surface checks
- introduce execution plan
- envelope agent task text specs
- Apply rustfmt to current main
- re-trigger
- centralize enum dispatch via methods on the enum
- remove/narrow dead code flagged by audit
- Normalize discovered workload JSON scenario IDs
- ratchet safety surfaces
- harden cache invalidation
- apply cargo fmt to unblock CI lint gate
- Normalize workload JSON bench scenario IDs
- Reject truncated Lab runner output
- dedupe summary-command JSON accessors into shared helper
- check docs manifest drift
- cache entity suggestions
- Format runner evidence files
- collapse redundant audit and output checks
- cache loaded config snapshot
- Expand rig extension settings for bench components
- normalize artifact refs
- centralize support summary
- move orchestration to core services for thin adapters
- purge ecosystem literals from upstream_workaround detector
- refactor(runner,release): dedupe near-duplicate + parallel-implementation helpers
- add PR profile
- reuse bot identity constant
- remove dead code + unused imports flagged by audit
- Index controller child evidence
- avoid eager startup probes
- add typed provider config envelope
- reduce quality gate work
- reuse changed file preflight
- add transport selector
- batch remote capability probes
- refactor(agent-task): split runner policy module
- cache hot git probes
- surface active jobs in status
- query artifact rows in sql
- extract lab offload workspace stage
- centralize command contract labels
- Persist bench matrix parent runs
- split god files + dedup + add remote-exec preflight
- Improve Lab runner readiness selection

### Fixed
- resolve thin command adapter finding (#5493)
- resolve failing test (#5494)
- DRY intra-method duplication to clear findings (#5491)
- guard against missing committed lockfile shipping stale git deps (#5426)
- stop lint-autofix from pushing to PR branch mid-review (#3819)
- strengthen vacuous command-surface test (#5479)
- resolve failing test (#5477)
- unify parallel implementations to clear findings (#5421)
- strengthen vacuous tests to clear test_quality findings (#5423)
- eliminate docs/changelog.md multi-PR conflict surface (#4876)
- report accurate deployed ref when HEAD is ahead of tag (#5456)
- reclaim target/ dirs for merged worktrees to prevent disk fill (#4875)
- allow bypassing preflight.package build-structure validation (#5425)
- treat CLEAN-with-zero-checks PR state as not-mergeable (#4872)
- Fix from-spec cwd fallback in snapshot workspaces
- Fix dispatch workspace fallback for stale cwd
- Fix bench run artifact diagnostics
- Fix controller dispatch cwd portability
- ignore failed inventory-only results
- Fix release workflow input reuse
- remove unused daemon_api_post re-export breaking lint
- add remote-exec path-translation + capability preflight
- resolve command audit findings
- Fix current-main test formatting
- Fix audit workflow test args
- add missing profile field to AuditRunWorkflowArgs test initializers (#5396)
- Fix artifact reference runner token parsing
- make status job enrichment tolerant of unreachable broker
- bound output capture with truncation metadata
- enforce reverse claim leases
- satisfy transport lint
- fix(agent-task): satisfy controller split lint
- annotate extracted offload stages
- satisfy contract label lint
- satisfy artifact query lint
- fix(api-jobs): update remote runner test claims
- format command surface test
- format reverse worker test
- export cached release helper
- keep reverse worker exec local
- align job store tests with claim leases
- format runner facade exports
- export job cancel through facade
- use runner facade for job cancel
- include claim id in daemon lifecycle test
- align worker test session fixture
- restore reverse cancel test compatibility
- materialize/resolve Codex provider plugin path for Codebox recipes
- bound command output capture with truncation metadata
- satisfy execution context clippy
- complete quality check compatibility
- unblock quality checks
- call cancellation helper from core
- compile cancellation command
- enforce concurrency limit when claiming jobs
- enforce remote job claim leases
- execute claimed jobs worker-local
- route job cancellation by transport

## [0.245.0] - 2026-06-19

### Added
- signal Lab auto-offload target and clarify remote failures (#3815)
- feat(agent-task): surface root-cause diagnostics in failure output (#3806)
- restart declared resident services after binary swap (#5197)

### Changed
- Document list command
- Normalize agent task provider contract fields
- reuse changed scope across stages
- extract core git intent helpers
- cache planning git probes
- purge ecosystem literals from core detectors + commands (#5232, #5233)
- add scoped audit groundwork
- centralize changed scope preflight
- introduce run service boundary
- mark display-only plan dependencies
- dedupe git_probe_path and path-resolution helpers (#5237, #5239)
- remove redundant dead-code markers and unused fns (#5250)
- centralize output file writing
- remove/narrow dead code flagged by audit (#5234)
- refactor(agent-task): split agent_task_controller_service god file + dedup (#5208, #5207)
- Resolve provider secrets before Lab run-plan offload
- reuse contract support copy
- extract offload workspace planning
- make active runner jobs opt-in
- cache standalone config fallbacks
- share JSON envelope descriptor
- reduce artifact IO overhead
- Refactor git process helpers
- cache explicit tag fetches
- Keep workflow abilities out of provider capability gates
- Execute loop retry and request changes actions
- Allow controller dispatch defaults at runtime
- cache git drift during status
- reuse central git helpers

### Fixed
- remove duplicate self-check changed-file fields
- add missing precomputed_changed_files to LintArgs/TestArgs initializers (#5266)
- Fix self check test args
- unblock base test bootstrap
- complete quality check compatibility
- unblock quality checks
- neutralize agent runtime package env
- require fresh reverse heartbeat
- install shared runtime agent CI assets
- import dependency kind in spec compiler
- keep runtime product names out of core
- support controller Codebox contracts
- set dependency kind on agent task steps
- keep run service import private
- import Path for review output writer
- cache upstream drift by git root
- avoid extension loading for root help/version
- preflight agent-task loop providers
- bound command output capture with truncation metadata (#5238)
- accept named refs after extension sync
- repair core_owned_source_stays_language_and_framework_agnostic after dependency-materialization comments (#5227)
- isolate per-run workspaces to prevent cross-run contamination (#4393)
- materialize worktree dependencies before verify gate (#3771)
- repair core_owned_source_stays_language_and_framework_agnostic failure (#5217)
- capture write-fix patch in lint/refactor offload results (#4315)
- fix(agent-task): align cook preflight with providers --runner resolution (#4860)

## [0.244.0] - 2026-06-19

### Added
- add deterministic loop primitives
- feat(agent-task): validate controller proof artifacts

### Changed
- refactor(agent-task): split structural god files + dedupe dispatch field group (#5181, #5187)
- Allow workflow dependencies outside artifact references
- test(agent-task): keep proof fixtures runtime neutral

## [0.243.0] - 2026-06-19

### Added
- feat(agent-task): materialize policy result inputs

## [0.242.0] - 2026-06-19

### Added
- feat(agent-task): compile artifact graph edges
- feat(agent-task): materialize loop specs
- feat(agent-task): expose controller events command
- compile controller loop fanout sections
- feat(agent-task): add repo-local gate executor
- compile repo loop specs into agent task plans
- feat(agent-task): classify and retry transient provider timeouts

### Changed
- refactor(command-contract): dedupe output descriptor field group
- trim filter audit footprint
- keep filter predicates on args
- skip remote probes for local filters
- refactor(agent-task): trim scheduler wait audit footprint
- refactor(agent-task): keep scheduler timeout helper internal
- perf(agent-task): wait until scheduler deadlines
- Warn on oversized patch releases
- cover hidden top-level parser commands
- remove product-specific runtime fixtures
- trim process snapshot audit footprint
- move artifact index summaries to common
- keep process watch polling on state
- batch rig artifact indexes
- share process snapshots across watchers
- Add control-plane agent tool dispatch
- route migrated JSON commands through adapters
- Add deterministic loop terminal outcomes
- build module surfaces from snapshots
- perf(agent-task): wake scheduler on cancellation
- route dispatch through registry
- remove dead code + fix adapter naming
- reuse codebase map snapshots
- perf(agent-task): reduce scheduler polling wakeups
- register json command families

### Fixed
- correct pipeline settings-env fixture to stay runtime-agnostic (#5184)
- capture liveness once to fix status_refresh race
- fix(agent-task): keep malformed provider output permanent
- use generic target artifact kind
- fix(agent-task): remove duplicate loop producer lookup
- surface extension command health

## [0.241.2] - 2026-06-18

### Changed
- split god files + dedupe field patterns
- purge org-specific literals — keep proxied-git support org-agnostic
- expose adapter output descriptors
- cover nested command surface
- batch artifact lookup
- reduce walker metadata overhead
- cache resource metric constants
- reuse entities on load misses

### Fixed
- reuse process snapshots across watchers

## [0.241.1] - 2026-06-18

### Changed
- Add agent task loop definition compiler

### Fixed
- reconnect stale runner daemons after upgrade
- tolerate missing cleanup paths

## [0.241.0] - 2026-06-18

### Added
- auto-clean self artifacts during cleanup

### Changed
- extract Lab orchestration and extension inference into core
- extract observation and artifact logic into core services

### Fixed
- catch partial Homeboy temp targets

## [0.240.0] - 2026-06-18

### Added
- feat(agent-task): add runtime materialization contract
- feat(secret-env): add deterministic plan diagnostics

### Changed
- Avoid generic secret env default helper
- Attach runtime materialization plans to providers
- Inline secret env resolver callback type
- Keep runtime manifest helpers internal

## [0.239.1] - 2026-06-18

### Changed
- resolve audit findings — dedup + remote-exec preflight (#5089, #5091, #5092, #5093)
- dedupe repeated field patterns + fix runs corpus test (#5090, #5088)

### Fixed
- expose bench settings to prepare commands
- exclude agnostic-registry files from core-agnostic-source policy (#5095)

## [0.239.0] - 2026-06-18

### Added
- include temp homeboy checkout targets
- include detached self temp artifacts
- add self artifact cleanup
- feat(agent-task): record patch promotion status
- detect merged-but-not-live code (merged → released gap)
- feat(agent-task): add generic tool control-plane contract
- feat(agent-task): resolve provider executable readiness
- feat(agent-task): export provider contract metadata
- feat(agent-task): add workspace materialization specs
- feat(agent-task): pass canonical secret env plans to providers
- resolve executable env aliases
- add declarative capability requirements
- add generic command execution plans
- feat(agent-task): materialize component worktree requests
- define artifact manifest descriptors
- add detector profile config
- centralize target resolution policy
- support runner-side service declaration for Lab offload
- add reusable phase templates
- add executable requirements
- materialize package template variants
- clarify apply change contract
- feat(agent-task): export core contract metadata
- feat(agent-task): expose provider capability contract
- add process path containment primitives
- add artifact evidence contract primitives
- feat(agent-task): declare timeout artifact contracts
- feat(agent-task): plan controller specs

### Changed
- refactor(agent-task,runner,lab): resolve audit findings — dedup, field patterns, preflight, thin adapters (#5069, #5068, #5067, #5071, #5077, #5078)
- test(core-agnostic): neutralize wordpress-plugin fixture kind to keep test-content baseline at 81 (#5075)
- refactor(agent-task,lab): resolve audit findings — dedup, dead code, field patterns, preflight (#5050, #5040, #5048, #5049, #5052, #5011)
- make git_sync private-remote test hermetic (#4980)
- extract module to reduce top-level item count (#4965)
- Add generic agent runtime contract mapping
- Formalize argv command invocations
- purge ecosystem literals from core detectors via config (#2842)
- add generic arg transform spec
- centralize runner support labels
- route text presentation explicitly
- generalize artifact input writing
- route fleet output through typed adapter
- remove dead code flagged by audit
- neutralize rig fixture names
- share runner config spec
- add secret env contract resolver
- refactor(agent-task): split god file and add compact status summary

### Fixed
- provision provider file secret sources
- fix(agent-task): normalize runtime typed artifacts
- improve rig dependency offload recovery
- omit xattrs from runner snapshots
- fix(agent-task): support provider command argv
- make source snapshot policy configurable
- Fix lab cook provider secret hydration
- fix(agent-task): preserve provider manifest metadata
- fall back when direct invocation cannot spawn
- repair runner-upgrade test regressions after #4956
- fix(agent-task): honor provider fallback secrets in lab
- fix(agent-task): use provider sources for readiness
- bound reviewer-facing artifact evidence
- fix(agent-task): read provider json secret sources
- realign source-built runner binaries by identity

## [0.238.0] - 2026-06-17

### Added
- bundle compare evidence across scenarios
- execute scripts through runner exec
- add declarative requirement steps
- add --run-id and warn on unknown setting keys (#4477, #4478)
- feat(agent-task): add typed artifact primitives
- add portable evidence manifest schema
- feat(agent-task): add secret env plan primitive
- add generic run lifecycle record
- feat(agent-task): add runtime selection envelope

### Changed
- extract core review service
- test(agent-task): cover nested provider secret sources
- reuse core git primitives
- centralize temp-dir and git fixture scaffolding (#4866)
- Preserve runner agent-task observability

### Fixed
- fix(agent-task): normalize artifact declarations
- fix(agent-task): resolve provider credential sources
- retry transient SQLite lock on runner exec persistence (#4863)

## [0.237.0] - 2026-06-17

### Added
- emit machine-readable run refs
- add shared artifact evidence refs

### Changed
- refactor(command-contract): simplify command output descriptors (#4869)
- simplify runner upgrade failure and path realignment (#4870)
- dedupe entity listing and write paths (#4867)
- share agent task argv editing
- share tool metadata registry
- consolidate agent-task argv materialization helpers (#4868)
- keep agent runtime fixtures generic
- share core git output helpers
- keep nested-tree replace tests runtime-agnostic
- make core detectors and deploy/release language-agnostic

### Fixed
- persist agent task recovery plans
- fix(agent-task): allow lab retry runs
- fix(agent-task): dedupe missing runner secret_env names across plan tasks (#4865)
- fix(agent-task): surface failure diagnostics
- fix(agent-task): preserve from-spec cwd context
- install agent runtimes into canonical root
- enforce agent runtime provider contracts
- pass default provider config to Lab agent tasks
- name lab runner in resource policy guidance
- preserve portable config precedence
- add plain helper path output
- repair stale runner path homeboy
- ignore generated source metadata dirt
- make agent runtimes canonical
- include lint findings in pr digest
- improve resource policy rerun guidance
- ignore ci artifacts for autofix sources
- recover from advanced remote and expose exact release body
- robust same-filesystem replace for nested package trees

## [0.236.1] - 2026-06-16

### Fixed
- preserve project component local path

## [0.236.0] - 2026-06-16

### Added
- discover AI runtime providers

## [0.235.1] - 2026-06-16

### Fixed
- materialize shared runtime assets
- resolve agent task default backend from policy
- fix(agent-task): require declared default backend

## [0.235.0] - 2026-06-16

### Added
- generic managed runner-source primitive + drift doctor checks

### Fixed
- preflight Lab agent-task runner secrets
- fix(agent-task): preserve controller dispatch workspace context
- inline provider-config @file before offload and accept --runner on lab commands

## [0.234.0] - 2026-06-16

### Added
- discover standalone agent runtimes

### Added
- generic, runtime-agnostic managed runner-source primitive that keeps an extension-declared git checkout synced on the Lab runner, plus a runner doctor check that warns when a managed source is missing, tracks the wrong remote, or resolves to a non-canonical checkout outside its declared root (#3818, #4140)

### Fixed
- resolve rig component paths to the runner-side materialized checkout during Lab offload so remote `${components.<id>.path}` checks no longer fail against the controller path (#3766)
- expand portable `~`/`~/...` rig dependency checkout roots to an absolute runner home path so the materialized remote path is never a literal `~` directory (#3767)
- snapshot a dirty/untracked Lab rig dependency working tree as an explicit overlay under `--allow-dirty-lab-workspace` instead of refusing it, with dirty-vs-clean provenance recorded in workspace metadata (#3765)
- surface declared-vs-resolved paths and actionable hints on remote rig check-file failures so unexpanded component-path tokens and unresolved `~` are diagnosable
- materialize rig component paths on the runner for offload (#3766, #3767, #3765)
- preserve job terminal status across daemon restarts

## [0.233.0] - 2026-06-16

### Added
- discover agent runtime manifests

### Fixed
- derive repo loop artifact dependencies
- match agent task providers by extension alias

## [0.232.5] - 2026-06-16

### Fixed
- preflight agent-task runner secrets
- reject local extension parity auto-sync

## [0.232.4] - 2026-06-16

### Fixed
- realign source runner binary path
- reconcile changed agent task specs

## [0.232.3] - 2026-06-16

### Fixed
- forward source path to runner upgrades
- allow write when checkout is dirty only with Homeboy CI artifacts
- diagnose stale lab provider binaries
- refresh lab runner on provider drift

## [0.232.2] - 2026-06-16

### Changed
- disable PR autofix explicitly

### Fixed
- guide dirty lab cook checkouts to worktrees
- surface npm stderr in package failure + retry transient failures (#3238)
- propagate nested executor terminal_state failures in run-plan (#4683)
- omit empty-URI artifact refs from agent-task status (#4400)
- allow write-mode refactor/lint-fix when the checkout is dirty only with Homeboy-owned CI artifacts so autofix-on-failure is not blocked by its own generated output (#4684)

## [0.232.1] - 2026-06-16

### Changed
- keep release auto-refactor advisory
- align post-merge failure expectations
- harden preview bootstrap forwarding regression

### Fixed
- enforce required controller artifacts
- reject parent-relative remote_path escapes at preflight (#3488)
- fail closed when release gates bypass Lab routing (#4603, #4605)
- report success:false when a release is skipped (#4316)
- surface code-production metrics in agent-task review/status
- restore main quality gates
- allow runner-hosted agent task recovery
- mark tunnel service cwd runner-resident
- mark runner-hosted agent-task execution
- keep agent-task inspection read-only
- stop lab offload using source self command
- require git checkout for offloaded patch agents
- store agent-task run status durably
- recover stale controller child runs
- preserve runner-side tunnel service paths
- redact runner exec environment output
- refresh stale runner daemon sessions
- keep agent task settings contract generic
- warn on stale symlinked extension clones after sudo upgrade
- forward agent task global settings
- surface degraded tunnel service status
- do not report cook success when all patch artifacts are empty (#4610, #4613)
- reject empty/incomplete agent-task cook results
- materialize agent task provider refs
- reject malformed agent-task totals in listings
- improve config set string UX
- accept artifact workflow dependencies
- accept keyed controller spec maps
- release lint preflight treats harness errors as warnings (#4586)
- preserve nested rig package roots
- refresh tunnel liveness after readiness
- fail agent task on nested executor failure
- stream preview client artifact bodies
- tolerate malformed agent task status records
- preflight lab agent-task providers
- block lab offload on stale runner drift
- distinguish agent task patch candidates
- detect lab agent-task provider drift
- skip extension-gated components in --check instead of aborting the whole pass
- reflect terminal runner exec handoff state
- clarify rig install reinstall flags
- clarify ready_to_deploy as git-state-only, not a target diff (#4588)
- github.release must fail when no release object is created (#3541)
- fail loudly when an artifact deploy can't be verified to land (#3608)
- infer controller spec workspace
- skip npm publish for private packages instead of failing on EPRIVATE
- retry version read-back to avoid false-negative on binary swap
- map workflow bench artifacts from origin

## [0.232.0] - 2026-06-15

### Added
- compile repo loop workflow contracts
- compile repo workflows through controller dispatch

### Changed
- prove generic controller from-spec resume

### Fixed
- forward preview bootstrap redirects
- surface lab runner homeboy binary refresh
- route agent-task auth status to runners
- execute generic controller actions
- write output envelopes atomically

## [0.231.0] - 2026-06-15

### Added
- initialize controllers from repo loop specs
- add guarded --retag to recover a stale release tag

### Fixed
- recover stale runner homeboy paths
- skip repeated failed release attempts
- materialize runner-local preview services
- skip npm publish for packages marked `"private": true` (EPRIVATE) instead of failing the release

## [0.230.0] - 2026-06-15

### Added
- support `artifact_path` override on version targets so deploy artifact verification resolves against the shipped path (e.g. compiled `build/<block>/block.json`) instead of the source bump path (`blocks/<block>/block.json`)
- preserve component contracts in agent tasks
- carry extension phase timing evidence

### Changed
- Add durable PR ownership loop
- Resolve runner secrets and force SSH exec
- Serve run artifact URLs from artifact origin
- report managed validation dependencies
- Fail fast on delegated runner failures in captured exec
- Refactor provider-specific Lab contracts out of Homeboy core defaults
- Support Lab offload for preview consumers
- surface waiting extension phases in review output
- add changed-scope provider contract
- Prove tunnel preview readiness
- Move ecosystem defaults out of core source
- Add agent task discovery commands
- Keep Lab cook task payloads out of argv
- move agent task dispatch into service
- Surface active Lab runner jobs in run views
- Refactor Lab routing contracts
- move runs artifact cleanup into service
- centralize command output and contracts

### Fixed
- ignore generated homeboy lab output in dirty guard
- persist trace compare artifacts
- make 'no deployable components' hint actionable
- Fix main release gates
- resolve artifact version verification against shipped path (#4614)
- preserve agent-task json during lab offload
- ignore empty agent-task patch candidates
- forward runner secret refs to daemon jobs
- Fix declared parser test result sidecars
- persist validation progress ledgers
- pin deploy cleanup build exit sentinel
- align release skip no-tag assertion
- degrade runner upgrades with unresolved PATH drift
- box lab status output
- keep running evidence out of failed state
- support runner-owned preview tunnel orchestration
- format lab follow-up assertion
- normalize runner job PATH on runner host
- Fix no-op release planning bump handling
- surface Lab status follow-up context
- add raw runner exec output
- scope runner secret env resolution to selected commands
- fix bench artifact reference contract
- persist lab handoff run at dispatch
- print lab offload handoff on detach
- classify incomplete agent-task executor results
- support runner agent-task review
- report detached durable lab dispatch
- prefer extension-owned defaults contract
- Fix Lab patch and release outcome reporting
- make cargo cli verification unambiguous
- warn on runner doctor homeboy path drift
- Fix Lab offload timeout recovery guidance
- defer Lab run-plan secrets to runner
- classify PR CI state before merge
- accept forced source upgrades with new build identity
- allow release worktrees at default tip
- resolve runner-side observation runs
- clarify allow-local-hot routing
- label persisted trace run ids after offload
- accept parser adapter stdout results
- reject shallow controller git bundle sources
- route agent-task auth status to runners
- include service tunnel ids in list output
- fail broken declared parser adapters
- normalize agent task provider artifact roles
- route test result parsing through declared adapters
- declare trace toolchain provenance in manifests
- stabilize main release gates
- split output descriptors from lab policy
- support manifest-declared release preflights
- support runner agent task provider discovery
- inline agent task prompt files for Lab offload
- refresh runner rig sources before offload
- normalize bench diagnostic producer sources
- accept bench diagnostic severity
- keep active runner job metadata generic
- accept bench diagnostic details aliases
- expose lab cook run id before execution
- add runner job log tailing
- accept bench diagnostic code aliases
- verify lab extension sync revisions
- isolate extension replace clone temps

## [0.229.11] - 2026-06-14

### Changed
- extract Lab trace fetch refs
- harden artifact facade exports
- harden command contract facade boundaries
- route Lab agent-task bridge through facades

### Fixed
- surface agent task cook handoff
- summarize agent task cook output
- allow lab status runner selector
- detect stale PATH homeboy shadows
- promote runner JSON errors in Lab failures
- normalize Lab promotion patch paths
- classify cook output quality
- materialize lab extension sync sources

## [0.229.10] - 2026-06-14

### Fixed
- route Lab extension sync locally
- surface managed Lab follow-up commands
- add lab offload runner doctor scope
- add lab extension sync command
- install active binary for source upgrades
- tighten release and project orchestration UX
- fix lab cook runtime drift diagnostics

## [0.229.9] - 2026-06-14

### Changed
- extract run/artifact observation service from CLI adapter
- Extract agent-task controller execution into core service
- split Lab offload into focused service modules (#4304)
- make core::agent_tasks/runners facades explicit API groups (#4306)
- split command_contract by domain (#4307)

### Fixed
- accept noop lab mutation patches
- fix Lab changed-since base materialization
- fix final runner path realignment

## [0.229.8] - 2026-06-14

### Fixed
- fix runner path realignment after upgrade

## [0.229.7] - 2026-06-14

### Changed
- guard dirty Lab git workspaces

### Fixed
- fix runner upgrade path realignment

## [0.229.6] - 2026-06-14

### Changed
- Extract agent-task service orchestration
- Narrow core access behind facade modules
- Refactor command output handling
- Introduce typed command adapter contract

## [0.229.5] - 2026-06-14

### Changed
- refactor cli runtime orchestration
- move command contracts out of cli surface

## [0.229.4] - 2026-06-14

### Changed
- Auto-sync stale runner extension parity
- Preserve detached runner git workspace revisions

### Fixed
- Fix invocation temp root user collisions
- exclude stale TypeScript build info from Lab snapshots

## [0.229.3] - 2026-06-13

### Changed
- Derive plan wrapper fields from HomeboyPlan
- Rename internal non-workflow plan structs
- Project agent task plans through HomeboyPlan
- Represent preview ingress plans with HomeboyPlan

### Fixed
- use vendor-neutral API examples

## [0.229.2] - 2026-06-13

### Changed
- project fanout and matrix through HomeboyPlan

## [0.229.1] - 2026-06-13

### Fixed
- add adaptive agent task concurrency
- drop stale controller action fallbacks
- serve workflow bench artifacts through public origins

## [0.229.0] - 2026-06-13

### Added
- orchestrate wpcom edit-page previews

### Fixed
- make preview consumer orchestration generic
- execute nested controller actions
- index workflow bench publication artifacts
- orchestrate generic visual compare evidence
- isolate runner parser tests from lab metadata
- add nested controller primitives
- execute controller pending actions
- materialize run-plan file specs for lab
- flag stale agent task controller actions
- fail closed controller runner policy
- allow trace compare to share rig lease
- pass settings to component bench scripts
- flag stale component checkout paths
- preserve Lab dependency source evidence
- keep extension updates local by default
- skip rig preflight during bench list discovery
- remap Lab JSON path settings
- enforce runner extension readiness parity
- preserve global runner for runs show
- defer drifted runner extension failures
- route runs list runner option
- accept runs list runner option
- complete native preview reverse channel

## [0.228.22] - 2026-06-12

### Fixed
- skip stale runner extension sync during upgrade
- scope runner extension sync by policy

## [0.228.21] - 2026-06-12

### Fixed
- preserve dotted bench matrix settings

## [0.228.20] - 2026-06-12

### Fixed
- store agent task secrets in global config

## [0.228.19] - 2026-06-12

### Fixed
- clarify runner secret env diagnostics
- support secret-safe runner env
- isolate runner process env
- scrub Lab WP Codebox env overrides
- cache public artifact URL validation
- generalize lab run idempotency guard
- keep mirrored Lab runs readable offline
- validate public artifact viewer URLs
- mirror timed-out Lab runner evidence
- stop duplicate Workflow Bench lab attempts

## [0.228.18] - 2026-06-12

### Fixed
- clarify ssh status and runner env diagnostics

## [0.228.17] - 2026-06-12

### Fixed
- remap Lab WP Codebox path settings

## [0.228.16] - 2026-06-12

### Fixed
- verify deploy artifact versions before upload
- resolve trace secrets once for child runs

## [0.228.15] - 2026-06-12

### Fixed
- clarify dirty Lab git sync remediation
- parse lab subcommands after global flags
- add keychain read diagnostics

## [0.228.14] - 2026-06-12

### Changed
- Keep doctor resources core agnostic
- Add preview asset fanout gate
- Add native preview tunnel client
- Add trace secret env hydration
- Keep trace preview lifecycle generic
- Stabilize native preview lifecycle test
- Add native trace preview lifecycle mode
- format selected bench parser tests
- warn on rig workload path expansion

### Fixed
- fix bench scenario discovery diagnostics
- add preview ingress operator plan
- add native preview tunnel auth claims
- add preview ingress tunnel server
- expose Homeboy build identity
- restart stale managed runner daemons
- project bench artifact viewer links
- preserve workspace ownership for root materialization
- surface artifact viewer links
- parse lab dispatch failures from stderr
- preserve branch for controller git sync
- route runner source git syncs through controller
- materialize runner source upgrades
- forward source upgrade options to runners
- scope bench discovery to selected scenarios
- preserve aggregate-only lab dispatch failures
- preflight trace public preview assets
- filter failed bench results before validation
- preserve lab agent-task dispatch failures
- allow lab extension updates
- filter selected bench scenarios before validation
- persist nested publication artifact refs
- fix trace preview port diagnostics
- surface advisory assertion failures
- fix trace target compare flag misuse

## [0.228.13] - 2026-06-11

### Changed
- keep publication artifact fixture generic

### Fixed
- fetch artifact-store publication refs
- index nested publication artifacts

## [0.228.12] - 2026-06-11

### Fixed
- persist Lab pre-dispatch agent task failures

## [0.228.11] - 2026-06-11

### Changed
- keep trace canonicality fixtures generic
- declare cwd materialization by provider

### Fixed
- preserve generic dispatch cwd validation
- accept installed trace extensions as canonical

## [0.228.10] - 2026-06-11

### Changed
- surface tracker provenance

### Fixed
- preserve nested rig paths in Lab offload
- initialize trace metrics in test results
- initialize trace metrics in remaining results
- repair trace metrics merge
- materialize codebox cook cwd as git checkout

## [0.228.9] - 2026-06-11

### Fixed
- fix rig check lab component path env

## [0.228.8] - 2026-06-11

### Changed
- Add trace metric compare guardrails

### Fixed
- fix trace compare canonical provenance
- fix trace parser producer fields
- fix browser evidence artifact variants
- fix agent-task failed provider evidence
- sync private lab workspaces through controller

## [0.228.7] - 2026-06-11

### Fixed
- clarify Lab runner wait timeout ownership
- clarify stale runner daemon recovery
- report bench warning child counts
- update bench artifact fetch hint
- reject non-git Codebox dispatch cwd
- allow lab agent-task inspection

## [0.228.6] - 2026-06-11

### Fixed
- harden runner upgrade sync diagnostics
- recompute trace exit after result processing
- install missing runner extensions during upgrade
- sync runners after same-version upgrade
- bundle agent task keychain secrets
- avoid upgrade runner flag collision
- honor parsed trace result status
- sync runner extensions during upgrade
- reject missing Lab extension revisions

## [0.228.5] - 2026-06-10

### Fixed
- infer Lab extension parity from agent-task providers
- reject stale Lab extension revisions

## [0.228.4] - 2026-06-10

### Fixed
- preserve trace compare proof target args
- allow cached up-to-date lab dependencies

## [0.228.3] - 2026-06-10

### Fixed
- preserve git refs for lab trace compare
- fail stale lab dependency worktrees

## [0.228.2] - 2026-06-10

### Changed
- allow homeboy artifact downloads

### Fixed
- fail unsafe lint fixer success states

## [0.228.1] - 2026-06-10

### Changed
- remove daemon listener race

### Fixed
- prefer Lab snapshots for rig bench offload

## [0.228.0] - 2026-06-10

### Added
- add agent task artifact loop primitives

## [0.227.9] - 2026-06-10

### Fixed
- tolerate cached Lab dependency fetch failures

## [0.227.8] - 2026-06-10

### Fixed
- select run-plan aggregate from mixed Lab stdout

## [0.227.7] - 2026-06-10

### Fixed
- keep Lab rig dependencies fresh

## [0.227.6] - 2026-06-09

### Changed
- Compare Codebox browser evidence metrics

### Fixed
- harden Lab run-plan path remapping

## [0.227.5] - 2026-06-09

### Fixed
- remap run-plan payload paths for Lab offload
- forward run-plan secrets for Lab offload
- surface in-flight lab daemon disconnects
- stage run-plan files for Lab offload

## [0.227.4] - 2026-06-09

### Fixed
- discourage dependency gate bypasses
- support Lab offload for agent-task run-plan

## [0.227.3] - 2026-06-09

### Changed
- name PR verification labels
- name PR body AI model fallback
- clarify PR verification bullets
- centralize PR body empty labels
- reuse proof gate status labels
- remove stale corpus import

## [0.227.2] - 2026-06-09

### Changed
- centralize proof runner labels
- centralize proof gate scope labels
- centralize CI-required proof gates
- surface lab offload proof metadata

## [0.227.1] - 2026-06-09

### Fixed
- cancel runner jobs on wait timeout

## [0.227.0] - 2026-06-09

### Added
- add agent-task loop controller state

## [0.226.1] - 2026-06-09

### Fixed
- clarify core agnostic boundary failures

## [0.226.0] - 2026-06-09

### Added
- add rig package checks and workload defaults

### Changed
- surface plan gate results in lab metadata
- normalize gate result plumbing

### Fixed
- keep lab prerequisites extension-owned
- add bench phase memory attribution
- preflight lab bench runtime prerequisites
- make bench lab routing fail closed

## [0.225.2] - 2026-06-09

### Changed
- anchor gates and proof in homeboy plans

### Fixed
- sync source-built provider CLIs for lab tasks

## [0.225.1] - 2026-06-09

### Fixed
- remap lab provider config file paths

## [0.225.0] - 2026-06-09

### Added
- add Homeboy proof envelope

### Changed
- keep Lab workspace fixtures generic

### Fixed
- make agent-task proof provenance explicit

## [0.224.1] - 2026-06-09

### Changed
- keep lab workspace fixtures generic

### Fixed
- support dotted bench setting overrides
- tighten Lab cook loop discovery
- keep agent task PR guardrails generic
- report iterator PR guardrails
- allow agent-task loop Lab offload
- resolve bench shared-state artifacts
- preserve artifact get output bytes
- capture bench memory and responsiveness evidence

## [0.224.0] - 2026-06-08

### Added
- expose normalized bench gate results

### Changed
- Lab offload: sync runtime_overlays sources to the runner

### Fixed
- fix bench workload status aggregation
- record clean trace toolchain provenance

## [0.223.1] - 2026-06-08

### Changed
- Lab offload: sync + remap provider-config local paths to the runner

## [0.223.0] - 2026-06-08

### Added
- add agent-task cook loop controller
- add reverse runner worker loop

### Fixed
- align reverse worker with broker result fields

## [0.222.23] - 2026-06-08

### Fixed
- expand runner dependency tilde paths

## [0.222.22] - 2026-06-08

### Fixed
- persist Lab trace dispatch records

## [0.222.21] - 2026-06-08

### Changed
- Add agent-task provider auth ownership

### Fixed
- require canonical trace evidence by default
- allow managed parent-relative deploy paths

## [0.222.20] - 2026-06-08

### Fixed
- Fix Lab rig dependency root mapping
- tighten rig runner diagnostics
- tighten agent-task runner cook lifecycle

## [0.222.19] - 2026-06-08

### Changed
- Migrate agent task gates to normalized results
- Add normalized gate result primitive
- Plan local-only lab routing for scoped hot commands

### Fixed
- preserve release CI env in test gates
- normalize validation dependency lifecycle ids
- include tempfile in runtime dependencies
- isolate validation dependency lifecycle
- rewrite agent task cwd for lab offload
- allow agent task dispatch lab runner

## [0.222.18] - 2026-06-08

### Changed
- Manage lab validation dependency lifecycle
- Block stale lab dependency bypasses

## [0.222.17] - 2026-06-08

### Changed
- Fail closed for bench validation dependencies

## [0.222.16] - 2026-06-08

### Changed
- Gate bench dependency hygiene

## [0.222.15] - 2026-06-08

### Changed
- support private agent-task completion gates
- enable differential audit gating

### Fixed
- publish prepared release tags

## [0.222.14] - 2026-06-08

### Changed
- Guard force-hot local Lab bypass
- project fleet exec into core plans
- remove built-in promotion workspace backend
- refactor agent task promotion workspace seam

## [0.222.13] - 2026-06-07

### Changed
- reuse gate log tail helper
- add agent task gate feedback loop

## [0.222.12] - 2026-06-07

### Changed
- Nest agent-task review helpers
- Use shared agent-task PR evidence args
- Extract agent-task review command helpers
- Extract agent-task PR body rendering
- Group agent-task PR evidence fields
- Add agent-task PR finalization plumbing

## [0.222.11] - 2026-06-07

### Changed
- Clean up gate evidence import
- Extract agent-task gate reporting module
- Add deterministic agent-task gate evidence

## [0.222.10] - 2026-06-07

### Changed
- Wire tunnel tests to changed-scope filter
- Match tunnel test module to changed-scope filter
- Keep service tunnel backend audit-clean
- Harden agent-task patch promotion
- Add provider-neutral service tunnel backend

## [0.222.9] - 2026-06-07

### Changed
- Refine preview lifecycle artifact metadata
- Harden managed preview artifact contract
- Use neutral core audit examples
- Add managed service preview artifacts
- Add comparison preview links

## [0.222.8] - 2026-06-07

### Changed
- Add agent-task async handle lifecycle surfaces
- Add durable agent-task cancellation
- Add generic agent task fanout primitive
- Add agent-task review workflow
- Add browser origin evidence for previews

## [0.222.7] - 2026-06-07

### Changed
- Replace dispatch workspace handling
- Remove agent-task dispatch transport routing

## [0.222.6] - 2026-06-07

### Changed
- Add managed service tunnel lifecycle

## [0.222.5] - 2026-06-07

### Changed
- Move runner snapshot excludes to policy
- add runner snapshot excludes
- Expose lab offload settings diagnostics
- Generalize remote execution preflight audit
- add trace A/B browser proof reports
- split runner doctor tests
- format rig expansion test
- split deploy effect version checks

### Fixed
- Fix lab offload rig refresh for trace commands
- fix runner snapshot tsbuild metadata
- fix git stdin handling
- fix extension script stdin handling
- fix trace browser proof CI drift
- restore main release gates
- scope trace lab runner tool inference
- skip primary rig dependency materialization
- map rig dependencies to lab workspace
- expand rig dependency roots for lab
- execute daemon jobs without runner config
- pass runner snapshot to daemon exec
- prevent recursive lab offload routing
- expand package root in rig specs
- surface lab daemon exec failures
- derive deploy status from applied effect
- require opt-in for lab fallback
- improve runner exec path diagnostics
- speed up facade audit detector
- terminate cancelled runner exec process trees
- preflight runner command dependencies

## [0.222.4] - 2026-06-07

### Changed
- own browser evidence schemas

### Fixed
- Fix browser evidence audit field grouping

## [0.222.3] - 2026-06-06

### Changed
- preserve typed lint settings

## [0.222.2] - 2026-06-06

### Changed
- reuse runner path normalization helper
- split runner extension parity helpers
- route runner exec through typed process primitive

## [0.222.1] - 2026-06-06

### Changed
- Converge bench artifacts with observation records
- centralize lint and audit workflow persistence

### Fixed
- route private runner source materialization through controller
- remove unused observed workflow helper
- Fix trace timeout cleanup evidence
- Fix core runner sidecar contracts

## [0.222.0] - 2026-06-06

### Added
- add trace public preview lifecycle

### Changed
- type issue reconcile plan actions
- keep trace public preview audit clean

## [0.221.2] - 2026-06-06

### Changed
- Define provider-neutral fanout boundary
- expect default resource namespace
- keep trace resource lease test scoped

### Fixed
- fix agent task executor adapter seam
- enforce trace rig resource leases

## [0.221.1] - 2026-06-06

### Fixed
- fix trace codebox artifact diagnostics

## [0.221.0] - 2026-06-06

### Added
- add agent task dispatch command
- add source policy audit primitive

### Changed
- refresh release lockfile fixture baseline
- Expose rig run artifact indexes
- Add browser evidence comparison report
- Keep test manifest filter config focused
- Wire trace workload invocation requirements
- Move test filter mapping into extension metadata
- Serialize observation store migrations
- Fail fast on noninteractive hot runs
- Allow audit baselines through lab offload
- Remove retired core agnostic baseline helpers
- Fail missing agent task output dependencies
- Harden test observation isolation
- Preserve agent task outcome outputs
- Deduplicate GitHub release repair guidance
- Split trace audit modules
- Update trace evidence report fixtures
- Refuse non-canonical trace toolchains
- Move bench compare tests beside implementation
- Keep trace runtime asset provenance generic
- Add trace toolchain provenance
- Add trace workload dependency preflight
- Add persisted bench comparison context
- Add audit baseline refresh command
- allow manual PR quality runs
- align dispatch module command shape
- split agent task dispatch command
- deduplicate dispatch task parsing
- Refresh trace artifact audit baseline
- Refresh trace matrix audit baseline
- centralize product identity literals
- add execution contract surface
- add trace scenario matrix runner
- refresh agnostic source policy baseline
- Refine bench phase event parsing
- add bench phase event persistence

### Fixed
- allow release prepare lockfile updates
- make release quality blocking configurable
- Fix Cargo test filter passthrough
- Fix bench parsing for extension sample metrics
- Fix small audit cleanup findings
- Fix release repair gate
- Fix GitHub release host proxy config
- Fix core agnostic baseline gate
- Fix release repair guidance
- Fix tagged deploy checkout restoration
- Fix release dirty phase attribution
- stabilize changed-scope guard checks
- Fix canonical trace refusal dependencies
- Fix core agnostic trace provenance fixture
- Fix canonical trace provenance fields
- Fix trace audit structure
- Fix trace preflight CI regressions
- preserve trace artifact directory evidence
- record declared trace artifacts once
- Fix trace observation artifact handling
- deduplicate audit config exclusions
- align dispatch run signature
- Fix trace evidence audit baseline
- Fix trace evidence audit findings
- Fix trace compare audit config merge
- Fix trace evidence audit config merge
- Fix trace artifact audit config merge
- Fix trace matrix audit config merge
- Fix trace evidence markdown build
- refresh runner audit baseline
- keep worktree metadata paths audit-clean
- split runner dependency materialization
- add scoped audit baseline sections
- route test audit detectors through descriptors
- clean runner materialization boundary terms
- keep trace aggregation audit-clean
- move namespace grammar defaults into profiles
- harden deploy version and status papercuts
- preserve nested trace artifacts
- materialize rig dependencies for Lab offload
- add trace baseline candidate compare
- render trace evidence reports
- enrich trace run aggregation
- Fix trace nested artifact preservation
- skip stale source baseline checks in changed scope
- Fix bench phase audit drift
- route agnostic baselines through audit policy
- run audit detectors through descriptors
- configure test wiring policy
- keep contract grammar defaults language neutral
- keep fleet check useful when live checks fail

## [0.220.3] - 2026-06-04

### Fixed
- fix deploy debris and pull drift warnings
- fix agent task output dag phases
- fix deploy archive extraction hints

## [0.220.2] - 2026-06-03

### Fixed
- fix stale lab runner daemon offload

## [0.220.1] - 2026-06-03

### Changed
- add agent-task queued run worker
- add component task worktrees
- support resolved component worktree agent tasks

### Fixed
- fix worktree audit findings
- fix agent-task promote source resolution
- fix relative deploy path roots

## [0.220.0] - 2026-06-03

### Added
- feat(agent-task): add deterministic smoke fixture

### Changed
- update audit baseline
- refresh runner lab agnostic baseline
- simplify lab route result
- preserve lab runner preflight after split
- simplify source grammar fixture
- isolate audit fixtures from user home
- refactor lab runner preparation tests
- split lab argv rewrite coverage
- keep timing spans core agnostic
- allow contextual artifact metadata fields
- drop fixed artifact portability baseline
- split lab offload helpers into modules
- refactor lab offload CLI routing
- test audit scoring behavior
- materialize linked shared scripts
- preflight stale lab sessions
- record detector timing spans
- test audit shared call floor behavior
- share source snapshot with structural detector
- optimize parallel implementation scoring
- move fingerprints into analysis context
- expose artifact portability scan cost
- run structural detector from snapshot
- install monorepo shared scripts
- isolate agent task command home
- keep generated artifact cleanup core agnostic
- agent-task: reject empty patch artifacts
- agent-task: reject empty timeout patches
- clean generated build artifacts

### Fixed
- reconcile core agnostic baseline count
- fix audit baseline artifact fingerprints
- keep Lab mutation offloads local without return support
- fix lab offload passthrough sentinel after split
- count artifact metadata candidates
- fix route result naming
- scope lab plan kind import to tests
- fix lint refactor dirty-file reporting
- fix lab offload passthrough sentinel
- fix agent task lifecycle audit drift
- fix output format footgun
- fix main quality gate regressions
- fix timeout artifact classification
- fix(agent-task): reconcile terminal status
- fix agent-task run-plan durable status
- fix release tag preflight ordering

## [0.219.1] - 2026-06-03

### Changed
- keep direct autofix commits without PRs
- disable release autofix PRs

## [0.219.0] - 2026-06-03

### Added
- support build artifact inputs
- summarize named spans in reports

### Changed
- trim release package component env assertion
- parallelize release quality gates
- refresh agnostic release baselines
- refactor(agent-task): extract timeout artifact discovery
- split daemon health classifier
- move lab bundle regression out of command file
- make deploy artifact zip fixtures hermetic
- keep reconciliation coverage compact
- isolate lab patch apply
- expand shared runtime helpers
- cover packaged stack preservation
- isolate lab validation dependency sync

### Fixed
- reject invalid release zip downloads
- fix release action component env
- fix(agent-task): let providers serialize timeout artifacts
- dedupe auth publish skip
- recover publish auth gaps
- fix(agent-task): satisfy lifecycle audit gates
- collect package artifacts from all providers
- fix(agent-task): recover stale submitted runs
- clean generated build artifacts
- derive github token for wordpress publish
- deduplicate artifact preflight failures
- create missing zip artifacts
- keep agnostic guard fixtures neutral
- reuse temp names for snapshots
- make self command prefix repo-owned
- decouple tests from node fixture
- guard concrete extension ids
- isolate lab snapshot workspaces
- flatten nested artifacts portably
- make source snapshot tests deterministic
- remove reverse broker listener startup race
- run Homeboy self PRs with candidate binary
- link generated notes to changelog
- fall back from stale lab daemon
- remap conflicting lab bundle run ids
- accept extension override for source runs
- stop duplicating generated release notes
- fix(agent-task): preserve timeout artifacts
- diagnose stale linked rig sources
- reconcile interrupted run metadata
- fix(agent-task): resolve declared secret env before dispatch
- avoid report audit drift
- apply lab write refactor patches
- type archive install manifest policy
- export bash preflight runtime helper
- declare structured sidecar producers
- add core test result adapters
- centralize lab workspace labels
- offload settings matrix runs to lab
- sync lab validation dependencies
- ship lab dependency workspaces
- merge explicit audit extension rules

## [0.218.0] - 2026-06-02

### Added
- feat(agent-task): run submitted lifecycle records

### Changed
- refactor(agent-task): keep lifecycle store path generic
- refactor(agent-task): split lifecycle persistence helpers

## [0.217.0] - 2026-06-02

### Added
- feat(agent-task): add durable lifecycle surfaces
- feat(agent-task): promote patch artifacts into worktrees
- preserve generic preview metadata
- feat(agent-task): add resource-aware fanout scheduling
- add settings matrix runner

### Changed
- refactor(agent-task): split scheduler plan types

### Fixed
- fix(agent-task): simplify lifecycle artifact collection
- keep command path fixtures core agnostic
- keep command path toolchain agnostic
- keep matrix args out of command root
- keep PATH helpers out of ssh client
- pass reference paths through workflow inputs
- model component selection as a plan
- reuse target resolution for local context
- preserve execution component overrides
- normalize lab command PATH
- refresh installed sources by package
- use GitHub generated release notes

## [0.216.2] - 2026-06-01

### Fixed
- fail zero-finding producer errors

## [0.216.1] - 2026-06-01

### Changed
- apply rustfmt

### Fixed
- keep structural complexity review-only
- refresh broken installed symlinks
- prevent plugin dir double-nesting + correct extract-command placeholder hint
- pass agent task provider config env
- route nested extension settings

## [0.216.0] - 2026-06-01

### Added
- dispatch extension agent task providers
- add bench matrix fanout command surface

### Fixed
- keep agent task provider helpers internal

## [0.215.2] - 2026-06-01

### Fixed
- import GitHub Actions artifacts by run id

## [0.215.1] - 2026-06-01

### Changed
- prove agent task matrix scheduler fanout

### Fixed
- render deploy artifact templates

## [0.215.0] - 2026-06-01

### Added
- add bounded sandbox tool API
- add agent task backpressure policy
- add agent task matrix fanout plan
- reconcile aggregate agent task outcomes
- add core agent task scheduler
- define agent task executor adapter
- add agent task workflow evidence

### Changed
- split agent task matrix module
- mark archive guard as validation
- guard deploy archive semantics

### Fixed
- reconcile agent task matrix merge
- keep archive verification selectors generic
- update outcome reconciliation after scheduler merge
- verify packaged plugin header files

## [0.214.0] - 2026-05-31

### Added
- add generic agent task schemas

### Fixed
- detect packaged plugin remote versions
- fence ssh runner exec as diagnostic

## [0.213.10] - 2026-05-31

### Changed
- genericize test coverage fixtures
- refresh detector agnostic baseline
- type observation run provenance

### Fixed
- clear observation context audit drift

## [0.213.9] - 2026-05-31

### Fixed
- flag stale deploy source checkouts
- type portable component config ownership

## [0.213.8] - 2026-05-31

### Changed
- guard core against Homeboy-domain leaks
- split test coverage behavior regressions
- split release executor helpers
- add change artifact apply contract
- document planned change execution contracts
- route review through plan executor
- reuse generic requested detector language parsing

### Fixed
- require canonical runner response envelopes
- keep split detector tests selectable
- keep execution mode helpers internal

## [0.213.7] - 2026-05-31

### Changed
- extract release package preflight
- centralize observation metadata finishing

### Fixed
- preflight release packaging before mutation
- clarify structured sidecar manifest contract

## [0.213.6] - 2026-05-31

### Fixed
- include audit findings in review artifacts
- scope targeted deploy component validation

## [0.213.5] - 2026-05-31

### Changed
- centralize finding and plan payload adapters

## [0.213.4] - 2026-05-31

### Changed
- remove lint finding pass-through adapter

## [0.213.3] - 2026-05-31

### Changed
- remove residual lint finding adapter
- remove legacy finding readers
- isolate runtime cleanup fixture
- split structural audit records
- keep bench budget fixtures generic
- refresh detector agnostic baseline

### Fixed
- retry transient refactor script spawn failures
- move lab offload extension parity into runner exec

## [0.213.2] - 2026-05-31

### Changed
- remove no-op lint finding adapter
- keep lint sidecar branch fixtures generic
- stop serializing legacy audit finding payload
- narrow annotation sidecar finding adapter
- extract refactor script spawn failure
- remove budget finding compatibility DTO

### Fixed
- normalize lint sidecar findings
- bound remaining output detail reporting
- hide runtime temp root helper

## [0.213.1] - 2026-05-31

### Fixed
- clear bench finding audit drift
- keep review artifact extraction focused
- normalize bench budget findings
- consume normalized review findings

## [0.213.0] - 2026-05-31

### Added
- add artifact cleanup command

### Changed
- route deps stack constraint test through integration target
- normalize audit findings output
- share plan execution loop

### Fixed
- add core artifact manifest substrate
- summarize lint producers in phase output
- avoid artifact path doc references
- pass stack dependency constraints through providers
- add core redaction substrate
- bound command output capture
- remove vacuous core agnostic tests
- remove legacy alias shims

## [0.212.0] - 2026-05-30

### Added
- split apply and publish contracts

### Changed
- use HomeboyFinding for test failures

### Fixed
- honor standalone deploy config fallbacks
- prefer named component worktrees
- isolate runtime env tests

## [0.211.0] - 2026-05-30

### Added
- add declarative rig file guards

### Fixed
- keep rig primitive checks generic

## [0.210.1] - 2026-05-30

### Fixed
- clarify lint changed-since help
- resolve registered worktrees from cwd
- reject unsafe lint autofix signature changes

## [0.210.0] - 2026-05-30

### Added
- add generic execution contracts

## [0.209.0] - 2026-05-30

### Added
- normalize runs finding outputs

## [0.208.0] - 2026-05-30

### Added
- add npm dependency provider

### Changed
- keep npm dependency provider isolated

### Fixed
- align npm provider with dependency update options
- keep npm provider tests on current stack edge shape

## [0.207.0] - 2026-05-30

### Added
- expose lint producer evidence

### Changed
- refresh core agnostic baseline

### Fixed
- align dependency update rebase defaults
- refresh dependencies after deps updates

## [0.206.0] - 2026-05-30

### Added
- expose invocation context JSON
- clarify structured sidecar manifest contract
- add typed sidecar writer dispatch
- add archive install deploy policy
- route changed tests from manifest config
- add shared runner prelude helper
- add shell bench scenario writer
- add read-only refs command

### Changed
- Preflight runner extension parity
- keep command surface guards current
- route dependency operations through provider seam

### Fixed
- split archive deploy manifest config
- avoid constant assertion clippy warning
- derive dependency stack edges from providers
- keep runtime helpers available without HOME
- allow extension deploy overrides for archives
- avoid routing source test modules directly
- add annotation sidecar writer helper
- narrow finding projection exports
- normalize test and budget findings

## [0.205.0] - 2026-05-30

### Added
- add shared Homeboy finding contract

### Fixed
- fail unverified registry publishes
- resolve finding contract audit drift

## [0.204.10] - 2026-05-30

### Changed
- keep build command fixtures core agnostic
- split lab rig materialization

### Fixed
- clear homeboy release blockers
- preserve rig declared path in bench snapshots
- reject unsupported component build_command
- preflight deploy artifacts before remote writes
- keep component set writes out of source config
- materialize rig benches for lab offload

## [0.204.9] - 2026-05-30

### Changed
- keep build fixtures domain agnostic

### Fixed
- prepare rig benches before timed workloads
- honor artifact build commands
- deploy released tag from release handoff

## [0.204.8] - 2026-05-29

### Fixed
- update project attachments on component path changes

## [0.204.7] - 2026-05-29

### Fixed
- batch observation finding inserts

## [0.204.6] - 2026-05-29

### Fixed
- honor explicit deploy build commands

## [0.204.5] - 2026-05-29

### Fixed
- capture audit-self subprocess memory

## [0.204.4] - 2026-05-29

### Fixed
- read persisted performance digest artifacts

## [0.204.3] - 2026-05-29

### Changed
- Accept runner-level bench evidence
- Expose target-adjacent deploy temp vars

### Fixed
- guard core-owned test fixture literals
- source cleanup artifacts from extensions
- move cargo target env into extensions
- share cargo targets across worktrees
- declare component cleanup artifacts

## [0.204.2] - 2026-05-29

### Changed
- split runner daemon refresh helpers
- Refresh stale runner daemons

### Fixed
- honor release script preflights

## [0.204.1] - 2026-05-29

### Fixed
- surface deploy source identity

## [0.204.0] - 2026-05-29

### Added
- add benchmark coverage and metric presets

## [0.203.0] - 2026-05-29

### Added
- add performance digest report

### Fixed
- promote output capture audit signal
- qualify runner metrics hash map

## [0.202.0] - 2026-05-29

### Added
- record runner process metrics
- render VPS broker deployment config

### Fixed
- use service-owned broker state paths

## [0.201.4] - 2026-05-29

### Fixed
- add reverse runner worker

## [0.201.3] - 2026-05-28

### Fixed
- route reverse runner exec through broker
- add reverse runner broker routes

## [0.201.2] - 2026-05-28

### Fixed
- stabilize lab offload metadata contract

## [0.201.1] - 2026-05-28

### Fixed
- move lab offload orchestration into core runner

## [0.201.0] - 2026-05-28

### Added
- declare lab command contracts

### Changed
- Add continuous loop archive sync triage

### Fixed
- keep lab capability planning out of main
- split lab command contract surface
- move lab capability planning into core runner
- preserve refactor source extension failures
- preserve Composer vendor in lab snapshots
- skip release artifacts for mutable deps

## [0.200.1] - 2026-05-28

### Changed
- Add benchmark evidence monitor
- Add private service tunnel declarations

### Fixed
- rebase reverse offload on tunnel substrate
- satisfy reverse runner audit gates
- support reverse runner hot offload selection
- correct dirty changed-since lab sync guidance
- redact runner env output

## [0.200.0] - 2026-05-28

### Added
- add runner trust policy
- add runtime sidecar writer helper

### Changed
- Add reverse runner job queue primitives
- Add reverse runner session substrate
- Require canonical extension manifest schema
- Require explicit portable component ids
- Remove legacy untagged output contracts
- Add issues reconcile-run command

### Fixed
- add id to release context test fixture
- require object rig workload specs
- trust homeboy for macos keychain items
- require canonical dynamic set inputs
- remove hidden compatibility flags
- require canonical cli argument forms
- remove legacy component manifest fields
- remove release positional bump shorthand
- remove standalone ssh runner compatibility
- preserve extension setup in runner snapshots

## [0.199.4] - 2026-05-27

### Fixed
- require directory project configs

## [0.199.3] - 2026-05-27

### Changed
- preflight runner extension parity
- guard detector implementation agnosticism
- test rig app update regeneration

### Fixed
- satisfy runner preflight gates
- restore core architecture guards
- pass formatter changed-file scope
- isolate remote artifact cleanup tests
- add homeboy artifact retention cleanup
- upgrade configured runners
- include git command diagnostics
- offload hot refactor runs to lab
- preserve parent-relative deploy paths
- preflight runner capabilities before remote exec
- prepare changed-since lab offload base
- spill large http egress bodies to artifacts
- audit artifact portability leaks
- prove runner artifact evidence paths
- detect unbounded detail output

## [0.199.2] - 2026-05-26

### Changed
- audit dry-run status contracts

## [0.199.1] - 2026-05-26

### Changed
- dedupe command descriptor shapes
- move observed workflow helper to utils
- anchor output contracts to public enums
- centralize command descriptor behavior
- Unify target resolution
- consolidate batch bulk output totals
- share observed quality workflow runner
- guard output variant contracts

### Fixed
- remove unused git root detector

## [0.199.0] - 2026-05-26

### Added
- generalize refactor source extension dispatch

### Changed
- Allow extension refactor source handlers

### Fixed
- split audit source filter helper
- scope extension refactor source dispatch

## [0.198.9] - 2026-05-26

### Changed
- extract bench run metadata helpers
- Reject changed-since Lab offload for snapshot workspaces
- extract extension execution helpers
- extract git operation helper modules
- extract config JSON helpers
- extract GitHub data contracts
- extract project file edit helpers
- extract defaults builtins
- split report failure digest helpers
- extract rig path helpers
- extract manifest config modules

### Fixed
- Fix runner doctor extension parity
- warn on runner doctor version skew
- clarify audit baseline summary counts
- make observation run imports idempotent

## [0.198.8] - 2026-05-26

### Changed
- Refactor runner offload preflight policy

## [0.198.7] - 2026-05-26

### Changed
- extract code audit execution plan
- Make artifact portability path policy configurable

## [0.198.6] - 2026-05-25

### Changed
- extract extension source metadata lifecycle
- extract release version target checks
- Refactor core fingerprint relationships
- extract grammar string scanner
- Refactor convention membership helpers
- extract test coverage symbol helpers
- Extract edit op import helpers
- extract runs bench helpers
- extract trace overlay lock command
- Extract trace aggregate test support

## [0.198.5] - 2026-05-25

### Changed
- Add output contract audit coverage
- Strengthen runner offload parity audit

## [0.198.4] - 2026-05-25

### Changed
- Document Lab offload support boundaries

## [0.198.3] - 2026-05-25

### Changed
- Record Lab offload provenance metadata

## [0.198.2] - 2026-05-25

### Changed
- Split bench report comparison helpers
- split duplication audit modules
- Split triage observation helpers

## [0.198.1] - 2026-05-25

### Changed
- extract decompose grouping planner
- split refactor source helpers
- extract GitHub PR comment helpers

## [0.198.0] - 2026-05-25

### Added
- auto-select default lab runner
- add Linux rig app launchers

### Changed
- Treat empty refactor transforms as success
- Cap refactor transform match details
- Add golden JSON contract fixtures for quality outputs
- remove unreferenced internal exports
- extract refactor source planning helpers
- add golden JSON command contracts
- extract triage watch helper

### Fixed
- auto-connect Lab runner before offload
- migrate legacy ssh runner configs
- align test coverage detector entry point
- gate Lab offload by runner capabilities
- bound self-check output capture
- Fix lab offload output path handling
- preflight lab runner test extensions
- persist observe artifacts cleanly
- report mirrored capture-patch artifacts
- write runner validation errors to output file

## [0.197.14] - 2026-05-25

### Changed
- cover http egress audit paths
- extract config JSON pointer helpers
- extract bench side-by-side report
- extract git operation output seam

### Fixed
- align docs with command help
- skip dry-run deploy plan for skipped releases
- include errored commands in failure digest

## [0.197.13] - 2026-05-24

### Changed
- extract observation store schema seam
- reject stale core agnostic baseline entries
- extract rename safety checks

### Fixed
- mark imported bundle artifacts metadata-only
- fail deploy when cleanup before extraction fails

## [0.197.12] - 2026-05-24

### Changed
- extract core fingerprint hashing
- extract github comment section helpers

## [0.197.11] - 2026-05-24

### Changed
- extract duplication suppression helpers
- extract refactor autofix commit helper

### Fixed
- surface broken extension symlinks
- trim refactor unreferenced exports
- reduce server connection duplicate block

## [0.197.10] - 2026-05-24

### Changed
- extract config roundtrip detector

### Fixed
- reduce release and rig duplicate blocks
- reduce duplicate audit slice G
- trim internal unreferenced exports

## [0.197.9] - 2026-05-24

### Fixed
- reduce local duplicate audit blocks

## [0.197.8] - 2026-05-24

### Fixed
- suppress macOS sidecars in lab snapshots
- sync lab offload target workspace

## [0.197.7] - 2026-05-24

### Fixed
- resolve runner doctor artifact root
- apply runner env to SSH probes

## [0.197.6] - 2026-05-24

### Changed
- reduce duplicate audit blocks

### Fixed
- hide lab runner flag from unsupported help
- Fix runner exec failure status
- project workflow payloads from plans
- align code audit detector conventions
- reduce core fingerprint duplicate blocks
- reduce codebase map duplicate blocks

## [0.197.5] - 2026-05-24

### Changed
- Derive stack sync preview from HomeboyPlan

## [0.197.4] - 2026-05-24

### Changed
- Simplify command output routing
- Extract shared source command plumbing

### Fixed
- reduce intra-method duplicate blocks
- derive audit detector selection from plan
- share rig json config loaders
- derive trace run identity from plans
- back release plan compatibility with HomeboyPlan

## [0.197.3] - 2026-05-24

### Changed
- Make SSH runners server capabilities

### Fixed
- block release planning on orphaned tags

## [0.197.2] - 2026-05-22

### Changed
- ratchet core agnostic baseline

### Fixed
- split code audit detectors

## [0.197.1] - 2026-05-21

### Fixed
- detect public registry metadata exposure

## [0.197.0] - 2026-05-21

### Added
- detect config roundtrip key asymmetry
- detect write-only config keys
- detect mutating resource access gaps

### Changed
- Warn on request redirect validation gaps

## [0.196.0] - 2026-05-21

### Added
- add correlated requested detector rules
- detect scoped proxy audit drift

### Fixed
- explain divergent release tag recovery

## [0.195.1] - 2026-05-21

### Fixed
- scope lint autofix to reported files

## [0.195.0] - 2026-05-21

### Added
- add CI context to command outputs

## [0.194.0] - 2026-05-20

### Added
- run bench CI profiles

## [0.193.0] - 2026-05-20

### Added
- run CI jobs through lint and test

## [0.192.0] - 2026-05-20

### Added
- run CI profiles during review

### Fixed
- clean deploy build artifacts

## [0.191.0] - 2026-05-20

### Added
- run declared CI jobs

### Fixed
- fail changed-since validation on stale deps

## [0.190.0] - 2026-05-20

### Added
- add ci profile listing
- add http egress probe
- add triage watch mode
- add named trace profiles

### Changed
- Persist component local path updates

### Fixed
- keep CI discovery core agnostic

## [0.189.0] - 2026-05-20

### Added
- support systemd trace attachments
- support explicit rename variants

## [0.188.15] - 2026-05-20

### Fixed
- let homeboy own release creation

## [0.188.14] - 2026-05-20

### Changed
- split JSON output dispatch
- plan command response execution

### Fixed
- render staged artifact in deploy verification

## [0.188.13] - 2026-05-20

### Changed
- decouple artifact output from response mode
- move output artifact execution into commands

## [0.188.12] - 2026-05-20

### Changed
- move raw execution into commands

## [0.188.11] - 2026-05-20

### Changed
- share raw output errors

## [0.188.10] - 2026-05-20

### Changed
- move response policy into commands

## [0.188.9] - 2026-05-19

### Changed
- move response execution into commands
- move json dispatch into commands
- move raw output routing into commands
- move output artifact handling into commands

### Fixed
- pin version override fixture pattern
- isolate runtime temp env
- normalize parent-relative remote paths
- push explicit release refs
- verify override installs before cleanup

## [0.188.8] - 2026-05-19

### Changed
- share code audit line lookup
- reuse rig test support helpers

### Fixed
- exclude artifacts from crate package

## [0.188.7] - 2026-05-19

### Changed
- reuse path segment sanitizer
- share process group liveness helper

## [0.188.6] - 2026-05-19

### Changed
- reuse process liveness checks

## [0.188.5] - 2026-05-19

### Changed
- share process liveness helper

## [0.188.4] - 2026-05-19

### Fixed
- publish crates through release pipeline

## [0.188.3] - 2026-05-19

### Fixed
- default triage to personal workload

## [0.188.2] - 2026-05-19

### Changed
- cover scope resolver helpers
- route component scopes through shared resolver

## [0.188.1] - 2026-05-19

### Fixed
- skip head artifact release worktree planning

## [0.188.0] - 2026-05-18

### Added
- add compiler warning extension contracts

## [0.187.4] - 2026-05-18

### Changed
- Refactor extension module boundaries
- reuse daemon pid helper

### Fixed
- allow head release artifacts in worktree
- add runner artifact cleanup

## [0.187.3] - 2026-05-18

### Changed
- reuse git short revision helper
- flatten release version module

### Fixed
- allow head release from detached tags

## [0.187.2] - 2026-05-18

### Fixed
- install extensions before release finish
- allow project component remote path overrides

## [0.187.1] - 2026-05-18

### Changed
- narrow wildcard module surfaces

### Fixed
- select homeboy binary in release finish

## [0.187.0] - 2026-05-18

### Added
- finish pipeline from tagged head
- attach state.artifacts to github.release output

### Changed
- make git reexports explicit
- make changelog reexports explicit
- report core agnostic baseline debt
- make version reexports explicit
- make server reexports explicit
- require explicit core paths
- guard core command boundary
- decouple HTTP analysis jobs from commands

### Fixed
- skip package during self-release prepare
- fail release command on failed steps
- exclude homeboy build artifacts from scans
- include dirty files in test drift diffs
- keep release diagnostics extension agnostic
- guard runner doctor Unix probes

## [0.186.12] - 2026-05-18

### Changed
- add trace span record builder

### Fixed
- support GitHub Enterprise release remotes

## [0.186.11] - 2026-05-18

### Changed
- add trace run record builder

## [0.186.10] - 2026-05-18

### Changed
- reuse observation test run builders
- reuse observation fixture builder

## [0.186.9] - 2026-05-18

### Changed
- reuse observation run builder

## [0.186.8] - 2026-05-18

### Changed
- add observation run builder

## [0.186.7] - 2026-05-17

### Changed
- add plan values builder

## [0.186.6] - 2026-05-17

### Changed
- reuse remaining plan step inputs

## [0.186.5] - 2026-05-17

### Changed
- reuse decompose plan step inputs
- reuse issue plan step inputs
- reuse dependency stack step inputs

## [0.186.4] - 2026-05-17

### Changed
- reuse stack sync step inputs

## [0.186.3] - 2026-05-17

### Changed
- reuse quality plan step helpers

## [0.186.2] - 2026-05-17

### Changed
- centralize scope command names

## [0.186.1] - 2026-05-17

### Changed
- reuse plan builders in trace
- Capture Lab runner fix patches

## [0.186.0] - 2026-05-17

### Added
- apply lab patches locally

## [0.185.0] - 2026-05-16

### Added
- offload hot commands to lab runners
- sync runner workspaces
- record runner source snapshots
- execute commands on runners
- add plan construction helpers

### Changed
- reuse plan step builders
- detect direct aggregate construction

### Fixed
- mirror runner evidence into local runs
- honor head deploys for release artifacts

## [0.184.0] - 2026-05-16

### Added
- bootstrap runner daemon over SSH tunnel

## [0.183.1] - 2026-05-16

### Changed
- Persist daemon job state

## [0.183.0] - 2026-05-16

### Added
- add runner doctor diagnostics

### Changed
- Expose daemon artifact retrieval endpoints

## [0.182.0] - 2026-05-16

### Added
- add runner registry

## [0.181.6] - 2026-05-15

### Fixed
- enforce deploy path root resolution

## [0.181.5] - 2026-05-15

### Changed
- finish generic audit and trace plans

## [0.181.4] - 2026-05-15

### Changed
- move workflow plans onto generic plan substrate

## [0.181.3] - 2026-05-15

### Changed
- derive release plan metadata from generic plan
- centralize component plan construction

## [0.181.2] - 2026-05-15

### Changed
- make release plan access explicit
- use generic plan steps in release
- align file read coverage with audit conventions
- satisfy project pin audit checks

### Fixed
- include file size in file read output
- add atomic project pin updates

## [0.181.1] - 2026-05-15

### Changed
- drive review with quality plan

## [0.181.0] - 2026-05-15

### Added
- add file mkdir command

### Changed
- extract quality plan substrate

### Fixed
- keep file mkdir audit-clean

## [0.180.1] - 2026-05-14

### Changed
- drive release plans with generic substrate

## [0.180.0] - 2026-05-14

### Added
- introduce generic plan substrate

## [0.179.10] - 2026-05-14

### Changed
- Resolve deploy paths through project roots

## [0.179.9] - 2026-05-14

### Changed
- extract release execution orchestrator

## [0.179.8] - 2026-05-14

### Changed
- extract release planner

## [0.179.7] - 2026-05-14

### Changed
- extract release context loading

## [0.179.6] - 2026-05-14

### Changed
- Add generic HTTP auth profiles

## [0.179.5] - 2026-05-14

### Changed
- move release changelog bootstrap policy

## [0.179.4] - 2026-05-14

### Changed
- extract release git preflight policy

## [0.179.3] - 2026-05-13

### Changed
- extract release quality preflight policy

### Fixed
- enable native macOS keychain backend

## [0.179.2] - 2026-05-13

### Changed
- move release worktree preflight policy

### Fixed
- accept audit extension override

## [0.179.1] - 2026-05-13

### Changed
- extract shared scope model

## [0.179.0] - 2026-05-13

### Added
- add dependency stack propagation
- restore keychain API auth variables

### Changed
- split dependency stack helpers
- extract release working tree planning
- cover keychain auth helpers
- extract release changelog planning

## [0.178.1] - 2026-05-13

### Changed
- extract release semver planning

## [0.178.0] - 2026-05-13

### Added
- support proxied form API requests

### Changed
- cover API client request helpers
- extract release planning skip policy

## [0.177.0] - 2026-05-13

### Added
- execute release bump policy preflight

### Changed
- move release skip gates into plan

## [0.176.1] - 2026-05-13

### Changed
- split linked extension update metadata

### Fixed
- update linked extensions from default branch

## [0.176.0] - 2026-05-13

### Added
- execute release preflights before planning

## [0.175.0] - 2026-05-13

### Added
- execute quality release preflights
- execute remote sync release preflight

### Fixed
- honor project CLI path for CLI tools

## [0.174.1] - 2026-05-13

### Fixed
- accept key=value set paths

## [0.174.0] - 2026-05-12

### Added
- execute working tree release preflight
- support push auth overrides

### Fixed
- Fix server connect false failure for per-command SSH
- derive passthrough flags from clap metadata

## [0.173.0] - 2026-05-12

### Added
- execute default branch release preflight
- execute release changelog finalization

### Changed
- Support pinned extension installs

### Fixed
- surface unresolved triage targets

## [0.172.0] - 2026-05-12

### Added
- expose release changelog contract
- expose release quality plan steps

### Fixed
- keep autofix inside changed scope

## [0.171.0] - 2026-05-12

### Added
- expose release bump policy plans
- plan release command preflights
- include recovery release plans

### Fixed
- Fix lint changed-file ergonomics
- mark missing phpunit discovery as skipped
- suggest declared extension for unknown override

## [0.170.0] - 2026-05-12

### Added
- return executed release plans

### Fixed
- wire nested audit coverage tests
- dedupe derived literal sites

## [0.169.0] - 2026-05-12

### Added
- support requested detector context exclusions
- support dead guard comment exemptions
- include major release gate plans
- include no-releasable release plans

### Fixed
- discount corpus-common call scaffolding
- exclude generic helper suffixes from leaf naming
- skip typeless files for typed conventions
- limit vacuous mapping checks to comments

## [0.168.0] - 2026-05-12

### Added
- execute release deploy from plan

### Changed
- Standardize bench budget findings

### Fixed
- scope rename duplicate identifier warnings

## [0.167.0] - 2026-05-12

### Added
- add managed SSH server sessions

### Fixed
- scope rename duplicate identifier warnings

## [0.166.0] - 2026-05-12

### Added
- dispatch release execution from plan steps
- add bench scenario span timelines
- expose release preflight plan steps

### Changed
- share observation timeline span primitives

## [0.165.2] - 2026-05-12

### Changed
- Reduce audit false-positive noise

## [0.165.1] - 2026-05-11

### Changed
- use reusable Homeboy workflow

## [0.165.0] - 2026-05-11

### Added
- configure observation artifact root

### Fixed
- keep path helpers internal
- render issue fallbacks from native outputs

## [0.164.1] - 2026-05-11

### Fixed
- target releases at tag commits
- recover releases from existing tags

## [0.164.0] - 2026-05-11

### Added
- add PR policy primitives
- expose daemon run findings

### Changed
- cover PR policy primitives
- satisfy audit coverage for run history API
- align source resolver coverage

### Fixed
- detect import references in Rust attributes
- fail git steps with unresolved conflicts
- include dirty files in changed-since scopes
- honor filtered findings in exit status
- ignore namespace-only import references
- keep namespace usage check agnostic
- share rig stack test fixture
- keep source fallback hint agnostic
- resolve source checkout from cwd

## [0.163.2] - 2026-05-09

### Fixed
- include upstream prs for forks

## [0.163.1] - 2026-05-08

### Fixed
- ignore unknown merge-state flaps

## [0.163.0] - 2026-05-08

### Added
- compare observation snapshots

### Fixed
- avoid repeated comparison key fields

## [0.162.0] - 2026-05-08

### Added
- record comment activity observations

### Changed
- update schema count expectations

### Fixed
- keep triage item store APIs internal
- reuse PR signal record fields

## [0.161.0] - 2026-05-08

### Added
- add extension prepare step

### Changed
- align summary helper test names
- cover split pipeline helpers
- split prepare helpers from pipeline

## [0.160.1] - 2026-05-07

### Changed
- format validation context regression test

### Fixed
- keep git status refresh docs ecosystem agnostic
- surface dirty file list and refresh stale git index before working-tree check

## [0.160.0] - 2026-05-07

### Added
- support --path for ad-hoc observation of unregistered systems

### Fixed
- record effective component path on bench output (#2362)

## [0.159.0] - 2026-05-07

### Added
- persist resource policy context with run metadata

## [0.158.2] - 2026-05-07

### Changed
- GH Actions artifact ingestor + generic corpus primitives

## [0.158.1] - 2026-05-07

### Fixed
- verify active binary after upgrade

## [0.158.0] - 2026-05-06

### Added
- extension-supplied trivial/plumbing call lists for parallel-implementation detector
- expose computed drift_files for autofix consumers

### Changed
- move repeated enum dispatch onto impl methods
- refactor(update-check): share cache primitives between cli and extension update checks
- unify recursive directory copy helpers
- extract shared files_match helper

### Fixed
- remove Homeboy-specific core smell detectors
- stabilize intra-method duplicate findings
- remove core ecosystem wording leaks
- move framework command scopes into extension config
- expect persisted run artifact paths
- remove deploy planning duplicate error block
- body-shape gate for parallel-implementation detector
- scope command_output_policy out of WP-CLI consumers
- store rig trace baselines outside component checkouts
- resolve HEAD version paths against git toplevel for monorepo components (#2327)
- always pgid+cleanup local command spawns
- preserve invocation artifacts before cleanup
- persist bench workload artifacts
- persist bench observation artifacts

## [0.157.1] - 2026-05-05

### Changed
- keep audit convention tag changes clean

### Fixed
- meet sockaddr_un budget with /tmp anchor and STATE_DIR leaf
- shorten runtime path to fit sockaddr_un budget
- separate audit conventions with opaque tags

## [0.157.0] - 2026-05-05

### Added
- enqueue analysis HTTP jobs
- add invocation isolation primitives
- expose daemon job inspection routes

### Changed
- satisfy invocation audit coverage

### Fixed
- include test quality in vacuous-only runs
- satisfy invocation cleanup audit
- recognize helper-backed test coverage
- track invocation child process ownership
- avoid vacuous test false positives
- avoid source build during release check
- remove review-only issue policy

## [0.156.2] - 2026-05-04

### Changed
- detect repeated enum dispatch contracts
- detect configurable core boundary leaks
- Detect repeated observation lifecycle scaffolding
- Add command output policy audit detector
- Share audit analysis for fixability planning
- Add extension setting plumbing audit detector
- detect parallel runner setup
- detect vacuous test wiring
- Refactor fingerprint variable metadata
- Validate component remote URLs
- Refactor command output policies
- enforce core-agnostic source boundary
- Centralize extension plumbing
- simplify review stage orchestration

### Fixed
- require stronger god-file audit signal
- require extensions for write validation and formatting
- move release ecosystem behavior to extension contracts
- generalize component runtime requirements
- make extension discovery metadata-driven
- parse bench force-hot after subcommand
- keep component registration pointers in sync

## [0.156.1] - 2026-05-04

### Fixed
- prevent orphan tags from dep-refresh release cascade (#2234)

## [0.156.0] - 2026-05-04

### Added
- accept --path to bypass component registry

## [0.155.2] - 2026-05-04

### Changed
- isolate rig lease coverage

### Fixed
- consume standard trace probes
- collect file watch events for fswatch attachments
- baseline temporal assertions
- clear artifact diagnostics audit drift
- address artifact diagnostics audit
- improve empty artifact path diagnostics
- lease rig resources during benchmark runs

## [0.155.1] - 2026-05-03

### Changed
- cover audit naming inflection helpers

### Fixed
- suppress low-value repeated data contracts
- filter parallel implementation plumbing
- close legacy review-only audit issues
- suppress orphaned test false positives
- suppress duplicate argument-tail noise
- accept behavior tests for file coverage
- identify implicit default baseline failures
- expand env tokens in exclusive resources

## [0.155.0] - 2026-05-03

### Added
- group compact summary findings

## [0.154.0] - 2026-05-03

### Added
- add temporal assertion shapes
- add standard probe library
- support fswatch attachments

### Changed
- cover weighted audit scoring
- cover aggregate percentile serialization

### Fixed
- ignore lifecycle dead guard contexts
- disambiguate repeated span events

## [0.153.0] - 2026-05-03

### Added
- expose observation run readers

### Changed
- Keep observation exports out of source tree
- Warn before hot resource commands

### Fixed
- include findings in observation bundles
- improve aggregate outlier report

## [0.152.0] - 2026-05-03

### Added
- emit process lifecycle deltas

### Fixed
- default to workspace view

## [0.151.1] - 2026-05-03

### Changed
- Expose read-only daemon API contract
- Guard lower release bump overrides

### Fixed
- fix lint autofix release-owned files
- update linked extensions on current branch
- reduce audit false positives
- summarize bench report artifacts

## [0.151.0] - 2026-05-03

### Added
- preserve bench scenario metadata

### Changed
- Add run metadata distribution reporting

## [0.150.0] - 2026-05-03

### Added
- support local attach targets

## [0.149.0] - 2026-05-03

### Added
- add trace probe substrate
- evaluate temporal assertions
- add generic shell trace runner

### Changed
- Support component self-hosted scripts

## [0.148.0] - 2026-05-03

### Added
- add passive observe command

## [0.147.0] - 2026-05-03

### Added
- add trace experiment guardrails

### Changed
- format trace rig exports
- keep trace guardrail modules audit-clean
- keep trace guardrail coverage focused

## [0.146.0] - 2026-05-02

### Added
- add trace experiment plans
- classify trace critical path spans

### Changed
- split trace experiment support
- keep trace classification audit-clean

### Fixed
- preserve trace aggregate fields after rebase

## [0.145.3] - 2026-05-02

### Fixed
- interleave trace compare variant runs

## [0.145.2] - 2026-05-02

### Changed
- split trace compare variant coverage

### Fixed
- target compare variants in multi-component rigs

## [0.145.1] - 2026-05-02

### Fixed
- allow named trace compare variants

## [0.145.0] - 2026-05-02

### Added
- support multi-component trace variants

### Changed
- cover trace overlay module

### Fixed
- keep trace tests on main
- refresh trace variant rebase
- keep trace variants audit clean
- adapt trace variants to latest main

## [0.144.0] - 2026-05-02

### Added
- compare observation run metrics
- add trace variant matrix runs
- add trace compare-variant experiment runner

### Changed
- split trace matrix helpers
- align trace coverage with main
- cover trace overlay markdown
- Support rig-declared bench metric gates

### Fixed
- satisfy audit for runs compare
- integrate trace matrix after rebase
- refresh trace compare-variant against main
- clear trace audit drift
- share component normalizer flags
- avoid trace normalizer audit duplicate
- integrate compare-variant with focused trace spans
- address trace compare-variant audit findings

## [0.143.1] - 2026-05-02

### Changed
- Bundle trace compare experiment artifacts
- support named variants in rig configuration
- Add latest observation query helpers

### Fixed
- address trace overlay audit follow-up
- satisfy trace overlay lock audit
- recover stale trace overlay locks

## [0.143.0] - 2026-05-02

### Added
- add trace focus span reporting
- add trace phase presets

### Changed
- cover extension source url manifest metadata
- Add extension repair workflow

### Fixed
- address trace audit findings
- surface persisted bench run history
- reconcile stale runs before listing
- clarify missing extension provider errors
- address audit findings for observations
- surface trace overlay touched files
- lock trace overlay runs per component
- persist audit findings in observations
- persist test observations as findings
- keep source metadata repair under audit threshold
- keep linked extension update audit clean
- satisfy linked extension upgrade audit
- group linked extension root updates
- keep source url repair in lifecycle
- repair extension source metadata
- Fix linked extension updates in feature worktrees
- show actionable audit findings in review comments
- refresh matching local rig installs
- support nested detail spans

## [0.142.1] - 2026-05-01

### Changed
- cover observation finding records

### Fixed
- prioritize compare deltas
- diagnose non-monotonic phase spans

## [0.142.0] - 2026-05-01

### Added
- add trace phase milestones
- unify finding sidecar mapping
- compare trace aggregate spans

### Changed
- avoid trace test helper duplication
- split trace command helpers

### Fixed
- accept trace summary envelopes in compare

## [0.141.2] - 2026-05-01

### Fixed
- resolve repeated trace span events

## [0.141.1] - 2026-05-01

### Changed
- isolate default baseline output coverage

### Fixed
- handle pathless bench artifacts in provider failures
- omit merge commits from generated changelogs
- surface default bench baseline expansion

## [0.141.0] - 2026-05-01

### Added
- support parallel rig bench comparisons
- surface bench URL artifacts

### Changed
- align bench diagnostic test naming
- cover URL artifact storage

### Fixed
- make bench diagnostics workload-owned
- handle pathless bench artifacts in reports

## [0.140.0] - 2026-05-01

### Added
- add side-by-side bench reports
- classify AI provider bench failures
- stream bench progress without stdout noise

### Changed
- Merge origin/main into classify-ai-provider-failures
- satisfy bench progress audit checks
- cover capability stderr progress path
- satisfy provider failure audit checks
- isolate runs reconciliation
- simplify capability output dispatch

### Fixed
- reconcile orphaned running runs

## [0.139.0] - 2026-05-01

### Added
- add observation pointers to review output
- feat(obs-store): persist lint findings

### Changed
- cover output helper methods

### Fixed
- fix(obs-store): satisfy lint findings audit

## [0.138.0] - 2026-05-01

### Added
- feat(obs-store): record audit and test command runs
- declare sidecar schema contracts

### Changed
- cover sidecar schema declarations

### Fixed
- preserve full output artifact with json summary
- avoid manifest audit churn
- fix(obs-store): satisfy audit for command observations

## [0.137.0] - 2026-05-01

### Added
- feat(obs-store): record review parent runs
- aggregate repeated span runs

### Fixed
- clean up trace runner process groups

## [0.136.1] - 2026-05-01

### Fixed
- tolerate tiny baseline deltas

## [0.136.0] - 2026-05-01

### Added
- feat(obs-store): add observation bundle export/import

## [0.135.0] - 2026-05-01

### Added
- feat(obs-store): add observation query CLI

## [0.134.0] - 2026-05-01

### Added
- persist observation runs

## [0.133.0] - 2026-05-01

### Added
- validate stack-backed component paths
- persist observation runs

## [0.132.0] - 2026-05-01

### Added
- feat(obs-store): add run and artifact records

## [0.131.0] - 2026-04-30

### Added
- feat(obs-store): add SQLite foundation

## [0.130.0] - 2026-04-30

### Added
- attribute extension child resources
- add span reports and baselines

## [0.129.0] - 2026-04-30

### Added
- add resource diagnostics
- write lint resource summaries

### Fixed
- expose json summary output

## [0.128.1] - 2026-04-30

### Fixed
- support path-only inspections

## [0.128.0] - 2026-04-30

### Added
- scope workload preflight checks

### Fixed
- update linked and extracted installs consistently

## [0.127.0] - 2026-04-30

### Added
- repair safe symlink drift

## [0.126.0] - 2026-04-30

### Added
- report symlink drift in status
- track materialized ownership state

### Fixed
- derive lease resources from rig spec

## [0.125.0] - 2026-04-30

### Added
- support rig-owned trace workloads

### Fixed
- avoid HOME-dependent test path
- allow path-only extension resolution

## [0.124.11] - 2026-04-30

### Fixed
- block non-advancing release tags

## [0.124.10] - 2026-04-30

### Fixed
- persist release checkout credentials
- clear release-blocking duplication drift
- fail git push step on push errors

## [0.124.9] - 2026-04-30

### Fixed
- persist release checkout credentials
- clear release-blocking duplication drift
- fail git push step on push errors

## [0.124.8] - 2026-04-30

### Fixed
- persist release checkout credentials
- clear release-blocking duplication drift
- fail git push step on push errors

## [0.124.7] - 2026-04-30

### Fixed
- persist release checkout credentials
- clear release-blocking duplication drift
- fail git push step on push errors

## [0.124.6] - 2026-04-30

### Fixed
- persist release checkout credentials
- clear release-blocking duplication drift
- fail git push step on push errors

## [0.124.5] - 2026-04-30

### Fixed
- persist release checkout credentials
- clear release-blocking duplication drift
- fail git push step on push errors

## [0.124.4] - 2026-04-30

### Fixed
- persist release checkout credentials
- clear release-blocking duplication drift
- fail git push step on push errors

## [0.124.3] - 2026-04-30

### Fixed
- persist release checkout credentials
- clear release-blocking duplication drift
- fail git push step on push errors

## [0.124.2] - 2026-04-30

### Fixed
- persist release checkout credentials
- clear release-blocking duplication drift
- fail git push step on push errors

## [0.124.1] - 2026-04-28

### Fixed
- remove named transform config support

## [0.124.0] - 2026-04-28

### Added
- enrich targeted issue reports
- render failure digests from command outputs

### Changed
- cover repeatable PR comment banners

### Fixed
- classify changed-since context findings
- suppress boundary DTO field-pattern noise
- close stale reconcile categories
- route changed WordPress files by runner

## [0.123.1] - 2026-04-28

### Fixed
- compact cross-rig summary output

## [0.123.0] - 2026-04-28

### Added
- support grouped metric output

### Fixed
- support reversed cross-rig order

## [0.122.0] - 2026-04-28

### Added
- add composer dependency workflow

## [0.121.1] - 2026-04-28

### Fixed
- scope convention outliers in changed audits
- honor rig scenario and iteration overrides

## [0.121.0] - 2026-04-28

### Added
- add package self-check commands

## [0.120.1] - 2026-04-28

### Fixed
- avoid linked source revision writes

## [0.120.0] - 2026-04-28

### Added
- emit machine-readable review artifact

### Fixed
- gate fixability computation

## [0.119.0] - 2026-04-28

### Added
- configure priority issue labels
- add rig-defined profiles

### Fixed
- flag dirty merge states for rebase

## [0.118.0] - 2026-04-28

### Added
- add warmup control
- include run metadata in results

### Fixed
- propagate scenario and run options to rig workloads

## [0.117.0] - 2026-04-27

### Added
- surface artifact index in reports
- add semantic metric gates

## [0.116.0] - 2026-04-27

### Added
- add scenario selector
- report active binary status
- summarize cross-rig variance

### Fixed
- surface cross-rig failure stderr
- isolate update-all package failures

## [0.115.0] - 2026-04-27

### Added
- install stack specs from packages
- add local job event model

### Fixed
- parse authenticated GitHub remotes

## [0.114.1] - 2026-04-27

### Fixed
- materialize legacy bench helpers

## [0.114.0] - 2026-04-27

### Added
- resolve test drift from metadata

### Fixed
- synthesize typed CLI placeholders
- list rig-declared workloads
- skip unrelated detector work for filtered runs
- scope Swift CLI argv synthesis

## [0.113.0] - 2026-04-27

### Added
- support manifest-driven auto flags
- extract stale CLI invocations from shell scripts
- support extension env detectors

### Changed
- keep install-for-component coverage in lifecycle
- isolate env-sensitive tests

### Fixed
- select behind-upstream components
- detect unwired nested Rust tests
- suppress generic repeated field pairs
- tune intra-method duplicate noise
- filter plumbing calls from parallel implementation scoring
- classify vacuous tests separately
- parse Rust parameter types correctly
- respect nested Rust import scopes
- derive Rust namespaces from file paths
- ignore CLI option docs in legacy comments
- tune structural count findings
- ignore external CLI invocations

## [0.112.0] - 2026-04-27

### Added
- resolve package-relative workloads
- publish shared runtime helpers
- delegate remote path inference to extensions
- guard active resource leases

### Changed
- keep command path coverage out of pipeline core

## [0.111.0] - 2026-04-27

### Added
- add read-only endpoint contract
- add local HTTP daemon MVP
- install configured component extensions
- add extension lifecycle step

### Fixed
- bootstrap runner toolchain path
- include nvm node bins in command path
- bootstrap command step toolchain path

## [0.110.0] - 2026-04-27

### Added
- add sync diff preview
- declare rig resources
- add no-spec-edit rebase verb

## [0.109.0] - 2026-04-27

### Added
- sync component stacks
- push materialized targets

## [0.108.0] - 2026-04-27

### Added
- support agentic result artifacts and run summaries

## [0.107.0] - 2026-04-27

### Added
- allow component bench config

## [0.106.0] - 2026-04-27

### Added
- support reporting remote override

## [0.105.0] - 2026-04-27

### Added
- derive PR next-action labels
- add workspace portfolio view
- add failing check drilldown

### Fixed
- use installed id for rig lookup

## [0.104.0] - 2026-04-27

### Added
- add --runs N for cross-spawn distribution math

## [0.103.0] - 2026-04-27

### Added
- render native outputs for reconcile
- update installed package sources
- centralize runner support contracts
- detect stale Homeboy CLI argument shapes
- detect stale Homeboy CLI invocations
- expose command surface registry
- detect weak test hygiene
- manage installed rig sources
- add dev app launcher
- detect facade passthrough classes
- install shareable rig packages
- expose failed tests in output
- add requested PHP drift detectors
- order pipeline steps by dependency graph
- add rig component matrix
- add scenario discovery
- add component set attention reports
- add variance-aware metric comparisons
- pass rig-declared workloads to runners
- support shared dependency paths
- default baseline rig — auto-upgrade single-rig runs into comparisons
- add measurement-phase tag (cold/warm/amortized) to BenchMetricPolicy

### Changed
- consolidate shared test fixtures
- disable PR auto-refactor job
- share top-N grouping

### Fixed
- flag vacuous test placeholders
- tighten detector precision
- key reconcile matches by category
- accept raw path positional targets
- centralize rig HOME isolation
- ignore inline Rust test fixtures in field patterns
- source false-positive rules from config
- normalize abstract signature declarations
- keep review-only findings out of issue filing
- harden local service supervision
- allow tagged deploys with unreleased head
- discover renamed workspace components
- classify runner infrastructure failures
- reject duplicate scenario ids
- preserve shared-state CLI flags
- restore shared-state CLI flags
- ignore nested field-shaped syntax
- clarify build and review setup guidance
- simulate missing changelog in dry run
- parse positional bump argument
- set restart_required only for source installs

## [0.102.0] - 2026-04-26

### Added
- squash-merge detection in status + sync subcommand (closes #1570, Phase 2 of #1462)

## [0.101.0] - 2026-04-26

### Added
- add bench-audit-self workload for homeboy bench dogfood

## [0.100.0] - 2026-04-26

### Added
- homeboy stack — combined-fixes branches as a first-class primitive

## [0.99.0] - 2026-04-26

### Added
- homeboy issues reconcile + git issue close/edit primitives

## [0.98.0] - 2026-04-26

### Added
- migrate discovery + reference fingerprinting onto CodebaseSnapshot (slice 2 of #1492)
- --setting-json flag for typed settings overrides

### Fixed
- wait-ready loop in service.health http_check

## [0.97.3] - 2026-04-25

### Changed
- cut synthesis pipelines (testgen, docs generate) and demote MissingTestMethod

## [0.97.2] - 2026-04-25

### Fixed
- --output works at any position, not just pre-subcommand (#1532)

## [0.97.1] - 2026-04-25

### Changed
- split upstream_workaround + comment_blocks into submodules

## [0.97.0] - 2026-04-25

### Added
- add upstream_workaround finding kind
- CodebaseSnapshot + FingerprintIndex primitives (slice 1 of #1492)

### Fixed
- apply --only / --exclude to read-only audit findings

## [0.96.0] - 2026-04-25

### Added
- patch step, external services, newer_than check

## [0.95.0] - 2026-04-25

### Added
- cross-rig comparison — same workload across multiple rigs (closes #1523)
- add --fix flag dispatching to the canonical refactor pipeline

### Fixed
- missing_test_method recognizes descriptive test names

## [0.94.1] - 2026-04-25

### Fixed
- count_body_lines counts actual body, near_duplicate honors trivial-method list

## [0.94.0] - 2026-04-25

### Added
- --report=pr-comment markdown renderer (closes #1509)
- --shared-state and --concurrency for multi-instance workloads
- homeboy git stack — read-only stack inspection
- scoped audit + lint + test umbrella (closes #1500)

### Changed
- skip signature_check tests when rust grammar missing
- serialize HOME env overrides with a module-local mutex

## [0.93.0] - 2026-04-25

### Added
- rebase, cherry-pick, --force-with-lease + rig GitOps

## [0.92.0] - 2026-04-25

### Added
- CWD detection + --path on every verb

## [0.91.0] - 2026-04-25

### Added
- support generic metric policies

## [0.90.2] - 2026-04-25

### Fixed
- cfg-gate symlink call in pipeline.rs (round 2 of #1496)

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
