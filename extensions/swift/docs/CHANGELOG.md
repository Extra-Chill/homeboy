# Changelog

All notable changes to the **swift** extension will be documented in this file.

## [2.8.4] - 2026-08-29

### Changed
- Converge Swift lint and register its runner smokes

## [2.8.3] - 2026-08-01

### Changed
- gate pull requests on every extension's declared self_checks

## [2.8.2] - 2026-07-29

### Fixed
- seed the declared lint.findings sidecar on clean runs

## [2.8.1] - 2026-07-28

### Changed
- drop retired manifest keys and dead validate scripts

## [2.8.0] - 2026-07-17

### Added
- add extension discovery composition metadata

### Changed
- share runner sidecar adapter loading

## [2.6.3] - 2026-07-06

### Changed
- Converge shell runners on shared harness
- Consolidate runtime helper smoke coverage
- drop dead per-language runtime helper shims
- Use core sidecar helpers in non-WP runners

### Fixed
- remove shared core helper fallbacks
- prefer runtime helpers in extension wrappers
- consume core test scope in runners
- make Swift setup capability-aware
- make coverage completeness audit advisory

## [2.6.1] - 2026-05-21

### Fixed
- normalize sidecar contracts across extensions

## [2.6.0] - 2026-05-19

### Added
- declare structured sidecar capabilities
- declare drift selection config

## [2.5.1] - 2026-04-27

### Fixed
- adopt shared resolve context

## [2.5.0] - 2026-04-27

### Added
- fingerprint source symbols

## [2.4.2] - 2026-04-27

### Fixed
- advertise script test runner

## [2.4.1] - 2026-04-27

### Fixed
- align manifest smoke with combined capabilities

## [2.4.0] - 2026-04-27

### Added
- add optional lint runner

## [2.3.0] - 2026-04-27

### Added
- add CLT-safe validation runner

## [2.2.0] - 2026-04-27

### Added
- add manifest discovery and audit grammar

### Fixed
- harden script test runner

## [2.1.0] - 2026-03-21

### Added
- independent component versioning and continuous release for each extension
- add ESLint runner, Swift module, and documentation

### Changed
- rename modules to extensions across repo
- migrate all module manifests to grouped capability structure

### Fixed
- seed changelogs with current versions for homeboy release

## [2.0.0] - 2026-03-21

### Added
- Initial independent component release
