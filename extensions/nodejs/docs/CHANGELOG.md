# Changelog

All notable changes to the **nodejs** extension will be documented in this file.

## [3.7.1] - 2026-08-30

### Added
- merge project release artifacts
- add extension discovery composition metadata
- pass bench workload context
- declared local-workspace dependency overrides with peer dedup
- add trace workload helper
- add package script bench helper
- share browser result shapes
- add typed workload setting helpers
- add workload utility helpers
- add invocation runtime helper
- consume manifest changed-test routing
- summarize browser trace bottlenecks
- declare ci reproduction profiles
- declare structured sidecar capabilities
- add declarative browser interactions
- add browser profile diff helpers
- add browser performance profiler
- declare lockfile_paths for autofix drift resolution
- add post-write validate and format scripts
- preserve bench scenario metadata
- add HTTP probe status history
- add trace probe helpers
- discover extra trace workloads
- add trace observation helpers
- add trace runner support
- allow bench workloads to return custom metrics
- support list-only scenario discovery
- run rig-declared extra workloads
- bench/test/lint/build runners

### Changed
- Converge nodejs runners and revive nine dead smokes
- Declare settings runtime helpers
- Single-source the settings helper on Homeboy core
- gate pull requests on every extension's declared self_checks
- drop retired manifest keys and dead validate scripts
- share runner sidecar adapter loading
- Add an extension-owned Playwright browser utility
- Support standalone Node bench workloads
- Resolve nodejs browser helpers locally
- Declare source sync excludes in manifests
- Declare extension tool diagnostics
- Add Node.js fuzz runner capability
- Test nodejs helper packaged shared assets
- Allow npm deps hydration without lockfile
- Harden nodejs deps lockfile errors
- Add nodejs deps provider contract
- Converge shell runners on shared harness
- Consolidate runtime helper smoke coverage
- Remove dead compatibility checks
- Declare extension contract producers
- drop dead per-language runtime helper shims
- Single-source shared shell runner libs to top-level scripts/lib
- Declare Node.js bench env setting
- Detect pnpm workspace roots for dependency installs
- Add generic seams for WordPress runtime backends
- Quarantine remaining Codebox runtime surfaces
- Decouple WordPress agent runtime selection
- Add deferred-init browser evidence helper
- Normalize browser bench workload results
- Add visual parity workload helper
- Add browser bench result helper
- Package project script helper with Node extension
- Share runtime helper fallbacks
- Add Node trace reporter helpers
- route fix results through typed sidecars
- use typed sidecar writer APIs
- Normalize extension fix result capture
- Use shared runner prelude in extension runners
- Add shared command capture helper
- Add generic settings accessors
- Add generic node trace bridge helpers
- Add browser page scenario bench helper
- Add Node.js bench artifact helpers
- Add generic project script helpers
- Use core sidecar helpers in non-WP runners
- share bash version preflight
- use shared Homeboy helpers
- purge runtime extensions, scope repo to project-type primitives

### Fixed
- support npm shrinkwrap lockfiles
- declare fuzz runtime helper
- honor runtime settings helper for fuzz
- share Playwright browser cache with runtimes
- do not run TypeScript tests with bare node --test
- forward workload progress
- Fix #2226: retire Homeboy compat surfaces
- Fix nested Node.js fuzz workloads
- Fix nodejs dev overlay browser result helper
- Fix nodejs helper symlinked extension parent
- Fix nodejs helper dev-overlay shared scripts path
- Fix Node.js project ecosystem adapter id
- keep project script helpers extension-owned
- remove shared core helper fallbacks
- prefer runtime helpers in extension wrappers
- publish lab bench artifact links
- surface node bench warning result counts
- consume core bash preflight helper
- expose helper discovery for Node workloads
- keep runner prelude self-contained
- install node deps for clean runner snapshots
- enrich node test failure sidecar
- enrich node lint findings sidecar
- record redirect locations in node trace probes
- route targeted test args
- preserve Nx Vitest timeout context
- run selected changed tests
- preserve bench artifacts
- allow disabling bench warmup
- import list-mode bench file helpers
- adopt shared resolve context
- run build runner under bash

## [2.2.0] - 2026-03-21

### Added
- independent component versioning and continuous release for each extension
- add cleanup_paths to wordpress, nodejs, and rust modules

### Changed
- Add audit config to Node.js extension (feature_patterns, labels, doc_targets)
- rename modules to extensions across repo
- migrate all module manifests to grouped capability structure
- convert module configs to snake_case keys
- Initial commit

### Fixed
- seed changelogs with current versions for homeboy release
- fixing bugs and updating things

## [2.1.0] - 2026-03-21

### Added
- Initial independent component release
