# Changelog

All notable changes to Homeboy Action will be documented in this file.

## [2.11.7] - 2026-08-05

### Fixed
- stream shard aggregation payloads

## [2.11.6] - 2026-08-05

### Fixed
- replay shard manifests with canonical result paths

## [2.11.5] - 2026-08-04

### Fixed
- preserve failed shard aggregation

## [2.11.4] - 2026-08-04

### Fixed
- stream shard inventories into jq

## [2.11.3] - 2026-08-04

### Fixed
- select nextest for Rust shards

## [2.11.2] - 2026-08-04

### Fixed
- provision nextest for Rust shards

## [2.11.1] - 2026-08-04

### Fixed
- authorize v2 updates by remote tip

## [2.11.0] - 2026-08-04

### Added
- aggregate deterministic test shards

### Changed
- account for shard workflow actions

## [2.10.11] - 2026-08-04

### Fixed
- resolve the action ref once so a mid-run release cannot break reconcile

## [2.10.10] - 2026-08-04

### Fixed
- name the failing provenance field, and stop failing silently

## [2.10.9] - 2026-08-04

### Changed
- Preserve structured Homeboy results across Action stages

## [2.10.8] - 2026-08-03

### Fixed
- drop actions: read, which disabled CI in every consumer

## [2.10.7] - 2026-08-01

### Fixed
- use GitHub App client IDs

## [2.10.6] - 2026-08-01

### Fixed
- fail candidate after terminal evidence

## [2.10.5] - 2026-08-01

### Fixed
- move action dependencies to Node 24

## [2.10.4] - 2026-07-31

### Fixed
- preserve phase result JSON

## [2.10.3] - 2026-07-31

### Fixed
- stop reporting an unmeasured command as a pre-existing failure

## [2.10.2] - 2026-07-31

### Fixed
- restore cache in differential phases

## [2.10.1] - 2026-07-31

### Fixed
- propagate reusable test timeout

## [2.10.0] - 2026-07-31

### Added
- make differential quality phases restartable

## [2.9.3] - 2026-07-30

### Fixed
- preserve CLI and CI result provenance

## [2.9.2] - 2026-07-29

### Fixed
- resolve the release branch on a detached HEAD instead of skipping green
- assert a release published, instead of that it exists

## [2.9.1] - 2026-07-28

### Fixed
- stop the differential gate laundering an equal-failure run into a green check

## [2.9.0] - 2026-07-28

### Added
- surface action phase progress

## [2.8.27] - 2026-07-28

### Fixed
- let the differential gate tell a timeout from a test failure

## [2.8.26] - 2026-07-28

### Fixed
- report the classified release failure instead of "Unknown error"

## [2.8.25] - 2026-07-26

### Changed
- check out full history for the self-test suite
- gate releases on the action's own shell tests

## [2.8.24] - 2026-07-26

### Fixed
- fail closed when a strict command leaks force-killed descendants

## [2.8.23] - 2026-07-26

### Fixed
- survive processes exiting during supervisor /proc scan
- validate command result envelopes against the command root

## [2.8.22] - 2026-07-25

### Fixed
- validate operation command results
- use pidfds for strict cleanup
- make supervisor cleanup race safe
- supervise strict command descendants
- verify command result containment before launch
- fail closed on uncontained action commands
- complete action liveness verification

## [2.8.21] - 2026-07-24

### Fixed
- bound baseline command liveness and cleanup

## [2.8.20] - 2026-07-23

### Fixed
- normalize refactor lint commands

## [2.8.19] - 2026-07-20

### Fixed
- call reconcile at its real path 'homeboy runs findings reconcile'

## [2.8.18] - 2026-07-20

### Fixed
- categorize review-prefixed audit/lint/test commands

## [2.8.17] - 2026-07-17

### Changed
- Remove generic autofix mutation paths

## [2.8.16] - 2026-07-17

### Changed
- Bound action finalization and capability probes

## [2.8.15] - 2026-07-15

### Fixed
- fail closed when review commands do not finish

## [2.8.14] - 2026-07-15

### Fixed
- detect scoped Homeboy placement

## [2.8.13] - 2026-07-13

### Fixed
- adapt action to Homeboy placement contract

## [2.8.12] - 2026-07-13

### Fixed
- use published CI action majors

## [2.8.11] - 2026-07-13

### Fixed
- bound action command execution

## [2.8.10] - 2026-07-12

### Fixed
- authorize CI review execution on warm runners

## [2.8.9] - 2026-07-05

### Fixed
- route quality gates through review umbrella

## [2.8.8] - 2026-07-05

### Changed
- Remove bare quality command paths

## [2.8.7] - 2026-07-05

### Changed
- use review quality command defaults

## [2.8.6] - 2026-07-04

### Fixed
- gate review quality commands differentially

## [2.8.5] - 2026-07-04

### Changed
- Support review quality command paths

## [2.8.4] - 2026-06-24

### Fixed
- only pass --i-know-ci-creates-the-github-release when the homeboy binary supports it
- pair --i-know-ci-creates-the-github-release with --no-github-release when RELEASE_SKIP_GITHUB_RELEASE=true

## [2.8.3] - 2026-06-23

### Fixed
- create GitHub Releases during self-release

## [2.8.2] - 2026-06-22

### Fixed
- reduce differential CI latency

## [2.8.1] - 2026-06-22

### Fixed
- classify red baselines in differential CI

## [2.8.0] - 2026-06-21

### Added
- support head artifact release completion

## [2.7.14] - 2026-06-20

### Fixed
- give the release failed-SHA marker manual + tooling-aware escape hatches

## [2.7.13] - 2026-06-20

### Fixed
- pass --apply on real CI releases that use --skip-checks
- clarify missing build-artifact errors and autofix push visibility

## [2.7.12] - 2026-06-18

### Fixed
- honor changed-scope differential counts

## [2.7.11] - 2026-06-15

### Fixed
- reconcile autofix PR reports with committed files

## [2.7.10] - 2026-06-08

### Fixed
- allow GitHub Actions hot-runner checks

## [2.7.9] - 2026-06-04

### Fixed
- print scoped failure reproduction commands

## [2.7.8] - 2026-06-03

### Fixed
- guard unsafe PR autofix commits

## [2.7.7] - 2026-06-03

### Fixed
- stage autofix source changes without report rows

## [2.7.6] - 2026-05-31

### Fixed
- expect WP Codebox setting names

## [2.7.5] - 2026-05-27

### Fixed
- delegate PR comment tooling footer to core
- use core failure digest renderer

## [2.7.4] - 2026-05-27

### Fixed
- stage reported autofix files

## [2.7.3] - 2026-05-27

### Fixed
- scope autofix stale binary guard

## [2.7.2] - 2026-05-26

### Fixed
- avoid artifact action bootstrap in quality jobs

## [2.7.1] - 2026-05-24

### Fixed
- guard unsafe autofix PR reports

## [2.7.0] - 2026-05-21

### Added
- import observation bundles

## [2.6.0] - 2026-05-18

### Added
- expose release-skip-publish and release-skip-github-release

### Changed
- cover release publish defaults

## [2.5.4] - 2026-05-16

### Fixed
- ignore stale PR command results after merge

## [2.5.3] - 2026-05-13

### Fixed
- report refactor source autofixes

## [2.5.2] - 2026-05-12

### Fixed
- restore scope input

## [2.5.1] - 2026-05-12

### Fixed
- split reusable CI quality checks

## [2.5.0] - 2026-05-11

### Added
- add reusable CI workflow

## [2.4.5] - 2026-05-11

### Changed
- delegate issue rendering to homeboy

## [2.4.4] - 2026-05-11

### Fixed
- scope action artifacts by runtime

## [2.4.3] - 2026-05-11

### Fixed
- defer release verification to tag workflow

## [2.4.2] - 2026-05-11

### Fixed
- avoid observation artifact name collisions

## [2.4.1] - 2026-05-11

### Fixed
- scope observation artifacts by job

## [2.4.0] - 2026-05-11

### Added
- delegate PR policy gates to homeboy core

### Fixed
- preserve policy gate script executability

## [2.3.1] - 2026-05-11

### Fixed
- ignore assetless latest releases

## [2.3.0] - 2026-05-10

### Added
- add PR policy merge gate

## [2.2.11] - 2026-05-07

### Fixed
- drop redundant tip-sync workarounds, defer to homeboy core

## [2.2.10] - 2026-05-07

### Fixed
- treat empty categorizer input as success when nothing failed

## [2.2.9] - 2026-05-07

### Changed
- Add PR comment suppression input

## [2.2.8] - 2026-05-06

### Fixed
- forward typed settings json

## [2.2.7] - 2026-05-06

### Changed
- bump github actions for node 24

## [2.2.6] - 2026-05-06

### Fixed
- consume drift_files from homeboy core instead of hardcoding

## [2.2.5] - 2026-05-06

### Fixed
- direct-push Cargo.lock alongside audit baseline

## [2.2.4] - 2026-05-05

### Fixed
- stop filing generic CI failure issues

## [2.2.3] - 2026-05-05

### Fixed
- align reconcile CLI invocation

## [2.2.2] - 2026-05-05

### Fixed
- exclude baseline from autofix PRs

## [2.2.1] - 2026-05-05

### Fixed
- commit baseline-only drift directly

## [2.2.0] - 2026-05-01

### Added
- upload observation bundles

### Fixed
- authenticate Homeboy git pushes
- fix(pr-comment): render split quality sections

## [2.1.3] - 2026-04-29

### Changed
- delegate release auth to checkout
- refactor(pr-comment): delegate rendering to homeboy review

### Fixed
- report detailed PR fix summaries
- fix(pr-comment): skip green no-op comments
- render finding-level fixability

## [2.1.2] - 2026-04-29

### Fixed
- fix(pr-comment): suppress failed-test text for passing runs
- prioritize audit PR blockers

## [2.1.1] - 2026-04-28

### Fixed
- self-release from checkout
- configure git push auth
- preserve JSON command results
- move v2 to released tag target
- refresh extensions on cache hits

## [2.1.0] - 2026-04-28

### Added
- expose bench as a CI command
- install configured component extensions

### Changed
- delegate release decisions to homeboy
- align v2 action channel

### Fixed
- keep failure cache outside checkout
- fix(pr-comment): render unknown command status as warning
- fail closed on malformed results
- remove duplicated action footer
- fall back when latest release asset is missing

## [1.17.3] - 2026-04-27

### Fixed
- route release commands outside quality results
- verify GitHub Release after release

## [1.17.2] - 2026-04-27

### Fixed
- keep reconcile policy in homeboy core

## [1.17.1] - 2026-04-27

### Fixed
- clarify zero-failure test comments

## [1.17.0] - 2026-04-27

### Added
- add opt-in differential gating

## [1.16.12] - 2026-04-27

### Fixed
- reduce comment noise

## [1.16.11] - 2026-04-27

### Changed
- use homeboy review report for comments

## [1.16.10] - 2026-04-27

### Changed
- bridge to homeboy issues reconcile (homeboy v0.99+)

### Fixed
- install extension node deps for playground tests

## [1.16.9] - 2026-04-25

### Changed
- migrate to sectioned PR comment primitive (closes #141)

## [1.16.8] - 2026-04-24

### Fixed
- scope orphan reconciliation to expected-commands

## [1.16.7] - 2026-04-24

### Changed
- migrate auto-file-issue and open-autofix-pr to `homeboy git` primitives

## [1.16.6] - 2026-04-23

### Fixed
- paginate issue fetch, deduplicate stale audit/test issues

## [1.16.5] - 2026-04-23

### Changed
- replace Python normalize_audit_json with jq

## [1.16.4] - 2026-04-23

### Fixed
- make autofix strictly opt-in, not opt-out

## [1.16.3] - 2026-04-23

### Fixed
- three auto-issue filing bugs causing stale counts

## [1.16.2] - 2026-04-22

### Changed
- strip duplicated guard logic, delegate to homeboy core

## [1.16.1] - 2026-04-07

### Fixed
- remove push retry loop from autofix commit pipeline

## [1.16.0] - 2026-03-29

### Added
- detect PHP/Node versions via homeboy component env

## [1.15.0] - 2026-03-29

### Added
- add fleet and deploy as first-class action commands

### Fixed
- remove composer.json and package.json config fallbacks

## [1.14.0] - 2026-03-28

### Added
- create GitHub Releases after successful version releases

## [1.13.7] - 2026-03-28

### Fixed
- include collected_edits and decompose_plans in autofix commit summary

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
