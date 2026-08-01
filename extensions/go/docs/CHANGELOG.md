# Changelog

## [1.8.3] - 2026-08-01

### Changed
- gate pull requests on every extension's declared self_checks

## [1.8.2] - 2026-07-29

### Fixed
- seed the declared lint.findings sidecar on clean runs

## [1.8.1] - 2026-07-28

### Changed
- drop retired manifest keys and dead validate scripts

## [1.8.0] - 2026-07-17

### Added
- add extension discovery composition metadata

### Changed
- share runner sidecar adapter loading

## [1.6.1] - 2026-07-06

### Changed
- Add extension shape lint
- Converge shell runners on shared harness
- Declare extension contract producers
- Use core sidecar helpers in non-WP runners

### Fixed
- consume core test scope in runners

## [1.5.2] - 2026-05-22

### Fixed
- make coverage completeness audit advisory

## [1.5.1] - 2026-05-21

### Fixed
- normalize sidecar contracts across extensions

## [1.5.0] - 2026-05-12

### Added
- declare structured sidecar capabilities

## [1.4.0] - 2026-05-06

### Added
- declare lockfile_paths for autofix drift resolution
- add post-write validate and format scripts

## [1.2.0] - 2026-04-27

### Added
- declare drift selection config

## [1.1.0] - 2026-04-25

### Added
- add Go support for Homeboy extensions

## 1.0.0

- Initial Go extension
- Adds Go CLI, build, lint, test, and fingerprint support
