# Changelog

All notable changes to Homeboy Action will be documented in this file.

## [1.2.0] - 2026-03-06

### Added
- aggregate split CI job comments into shared PR comment with section keys

### Refactored
- organize action scripts into domain directories

### Fixed
- surface actionable audit findings in PR comments
- centralize homeboy command path handling in CI
- always post failure digest as review fallback

## [1.1.1] - 2026-03-05

### Fixed
- make PR comment and inline review publishing best-effort for fork PR tokens (avoid failing checks on 403)

## [1.1.0] - 2026-03-05

### Added
- add compact CI failure digest with top failed tests and audit findings in job summary and PR comment
- add capability probe for `test-scope: changed` with automatic fallback to `full` when Homeboy test changed-since support is unavailable
- add `test-scope-effective` action output and PR comment note showing resolved test scope
- document recommended two-workflow CI profile (PR scoped checks + main full suite with auto-issue)
- add tooling metadata capture (Homeboy CLI version, extension source/revision, action ref) and include it in failure digest, PR comments, and auto-filed issues
- add primary-vs-secondary failure sections with triage order in auto-filed CI issues

### Refactored
- decompose composite action logic into scripts and add Homeboy metadata/changelog/version files

## [1.0.0] - 2026-03-05

### Added
- initial public release of homeboy-action composite GitHub Action

### Changed
- no-op baseline entry for first tracked release
