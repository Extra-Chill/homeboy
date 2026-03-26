# Changelog

All notable changes to Homeboy Action will be documented in this file.

## [1.13.6] - 2026-03-26

### Fixed
- guard release and autofix against stale branch state

## [1.13.5] - 2026-03-25

### Fixed
- recheck synced PR head before autofix push

## [1.13.4] - 2026-03-25

### Fixed
- open non-PR autofix PRs without reruns

## [1.13.3] - 2026-03-25

### Fixed
- skip PR autofix on bot-authored head commits

## [1.13.2] - 2026-03-25

### Fixed
- skip circular rerun for refactor-only commands and bail early on merged PRs

## [1.13.1] - 2026-03-25

### Fixed
- skip autofix when a previous autofix was reverted on the branch

## [1.13.0] - 2026-03-24

### Added
- capture --output in PR autofix and include fixer categories in commit messages

### Fixed
- remove dead validate_autofix_changes call from non-PR autofix

## [1.12.1] - 2026-03-24

### Fixed
- surface refactor command failures in CI failure issues

## [1.12.0] - 2026-03-23

### Added
- add merge guard to skip autofix and PR comments on merged/closed PRs

## [1.11.3] - 2026-03-23

### Changed
- resolve merge conflicts — keep validation removal
- remove validate_autofix_changes — validation belongs in PR CI

### Fixed
- add compilation validation gate before committing autofix changes

## [1.11.2] - 2026-03-23

### Fixed
- add compilation validation gate before committing autofix changes

## [1.11.1] - 2026-03-21

### Fixed
- trigger release on push to main instead of cron

## [1.11.0] - 2026-03-21

### Added
- multi-component support via component subdirectory resolution

## [1.10.1] - 2026-03-21

### Fixed
- use stable branch name for non-PR autofix to prevent duplicate PRs

## [1.10.0] - 2026-03-19

### Added
- render per-kind autofix status in categorized issue bodies

## [1.9.1] - 2026-03-19

### Fixed
- remove `local` keyword outside function in prepare-autofix-branch.sh

## [1.9.0] - 2026-03-18

### Added
- extend categorized auto-issues to cover lint and test findings

## [1.8.0] - 2026-03-17

### Added
- detailed autofix commit messages with per-category fix counts

### Changed
- remove action-side validation — homeboy validates internally

## [1.7.1] - 2026-03-17

### Changed
- remove action-side validation — homeboy validates internally

### Fixed
- validate autofix changes compile before committing (#832)

## [1.7.0] - 2026-03-17

### Added
- include finding categories in autofix PR body and commit message

### Fixed
- baseline update bypasses autofix commit cap (#815)

## [1.6.1] - 2026-03-15

### Fixed
- scope autofix loop guards to actual loops, not historical totals

## [1.6.0] - 2026-03-15

### Added
- add continuous release workflow

## [1.5.3] - 2026-03-10

### Fixed
- restore real refactor command support

## [1.5.2] - 2026-03-10

### Fixed
- clarify autofixability messaging

## [1.5.1] - 2026-03-10

### Fixed
- remove `refactor ci` drift while preserving real `refactor ...` command support

## [1.5.0] - 2026-03-10

### Added
- informative autofix commit messages with fix types and file list
- enable audit --fix --write on PR autofix path
- deduplicate tooling versions and show autofix summary in PR comments
- auto-close audit issues when findings reach zero
- add binary-path input for build-once CI patterns
- delegate release to homeboy CLI (#56)
- add autofix-mode input for always-on baseline auto-ratchet
- use homeboy-ci-bot identity for all CI commits
- support GitHub App token for autofix pushes
- auto-ratchet audit baseline in autofix commits
- categorized audit issue filing

### Changed
- use structured output in release path
- consume Homeboy structured output directly
- derive autofix commands from supported Homeboy commands only
- unify scope logic into scripts/scope/ module
- remove extension revision workaround (belongs in homeboy core #639)
- consume structured JSON instead of scraping logs (#57)
- remove inline review annotations — redundant with PR comment

### Fixed
- enforce homeboy-ci identity for commits and release pushes
- default test-scope to 'changed' and fix misleading PR comment
- resolve extension revision for monorepos and enforce canonical command order
- pull latest before running release
- use app token for PR comments, issues, and autofix PRs
- scope autofix commit count to PR branch only
- skip baseline update on PR autofix commits
- scope baseline update to changed files in autofix
- strip PR references and scope tags from changelog entries
- revert manual changelog entry — handled at release time
- aggregate Cargo test results instead of taking last line
- rename bot identity from homeboy-ci-bot to homeboy-ci
- update audit issue body instead of adding comments
- remove collateral damage from inline review, add inline-review input
- remove redundant audit category labels from auto-filed issues
- changelog generation uses direct file ops instead of homeboy CLI

## [1.4.0] - 2026-03-06

### Added
- CI-driven continuous release pipeline — fully automatic version bump, changelog generation, and tagging from conventional commits
- Rewritten README with full release documentation and examples

## [1.3.0] - 2026-03-06

### Added
- add release command support — CI-owned version bump, changelog generation from conventional commits, tagging, and publish via homeboy release
- add release command support for CI-owned version management
- release command support for CI-owned version management with conventional commit changelog generation
- add release command support (#46)

## [1.2.2] - 2026-03-06

### Fixed
- fetch base ancestry for scoped three-dot diffs — progressive deepening eliminates noisy fallback warnings

## [1.2.1] - 2026-03-06

### Fixed
- fix bash brace expansion bug in generate-failure-digest.sh — ${RESULTS:-{}} appended extra } to JSON, silently breaking all PR comment failure detail sections
- add PHPUnit testdox format failure detection to test parser
- add JSON error patterns to test raw_excerpt extraction

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
