# Changelog

All notable changes to the **wordpress** extension will be documented in this file.

## [2.113.1] - 2026-06-01

### Fixed
- preserve Codebox timeout artifacts

## [2.113.0] - 2026-06-01

### Added
- add WP Codebox runner doctor

## [2.112.1] - 2026-06-01

### Fixed
- add WP Codebox cache refresh helper
- support Codebox bench bootstrap files

## [2.112.0] - 2026-06-01

### Added
- pass through Codebox runtime overlays

### Fixed
- exempt nonstandard PHP fixers from audit conventions

## [2.111.1] - 2026-06-01

### Fixed
- respect component PHPStan config

## [2.111.0] - 2026-06-01

### Added
- pass Codebox runtime stack mounts

## [2.110.9] - 2026-06-01

### Fixed
- emit current lint finding sidecars

## [2.110.8] - 2026-06-01

### Fixed
- mount rig bench workloads in Codebox

## [2.110.7] - 2026-06-01

### Fixed
- pass Codebox provider paths from task config

## [2.110.6] - 2026-06-01

### Fixed
- make phpcs-ignore-fixer string-literal-aware so phpcs:enable is never injected into multi-line SQL

## [2.110.5] - 2026-06-01

### Fixed
- preserve Codebox timeout evidence
- bound Codebox agent task execution

## [2.110.4] - 2026-06-01

### Changed
- harden Codebox Codex secret redaction
- prove Codebox Codex task requests

## [2.110.3] - 2026-06-01

### Fixed
- route WordPress JS smoke test selections

## [2.110.2] - 2026-06-01

### Fixed
- pin eslint config resolution in worktrees

## [2.110.1] - 2026-06-01

### Changed
- add codebox agent task matrix smoke

## [2.110.0] - 2026-06-01

### Added
- add Codebox agent task executor provider

## [2.109.2] - 2026-06-01

### Fixed
- select plugin archive headers by target basename

## [2.109.1] - 2026-06-01

### Fixed
- allow agent CI issue reads

## [2.109.0] - 2026-05-31

### Added
- consume codebox diagnostics artifacts

## [2.108.15] - 2026-05-31

### Fixed
- reserve issue writes for app token

## [2.108.14] - 2026-05-31

### Fixed
- tag app token for issue writes

## [2.108.13] - 2026-05-31

### Fixed
- tag agent CI GitHub credentials

## [2.108.12] - 2026-05-31

### Fixed
- omit empty agent CI app profiles

## [2.108.11] - 2026-05-31

### Fixed
- reserve app token for extra repos

## [2.108.10] - 2026-05-31

### Fixed
- separate agent CI token env vars

## [2.108.9] - 2026-05-31

### Fixed
- prefer repository token for agent CI

## [2.108.8] - 2026-05-31

### Fixed
- request app token write permissions

## [2.108.7] - 2026-05-31

### Fixed
- fail agent ci on reported errors

## [2.108.6] - 2026-05-31

### Fixed
- activate agent CI runtime plugins

## [2.108.5] - 2026-05-31

### Fixed
- quiet skipped phpunit discovery

## [2.108.4] - 2026-05-31

### Fixed
- report datamachine agent auth mode

## [2.108.3] - 2026-05-31

### Fixed
- support file mounts in agent ci recipes

## [2.108.2] - 2026-05-31

### Fixed
- support static-source wp-codebox benches

## [2.108.1] - 2026-05-31

### Fixed
- label phpcs lint summary

## [2.108.0] - 2026-05-30

### Added
- share browser result shapes

## [2.107.0] - 2026-05-30

### Added
- add bootstrap timeline profiler helpers

## [2.106.1] - 2026-05-30

### Fixed
- remove legacy Playground runtime aliases

## [2.106.0] - 2026-05-30

### Added
- adapt page profiler to Codebox browser artifacts

### Changed
- cover Codebox admin page artifacts

### Fixed
- route PHP traces through WP Codebox
- require explicit fixture host route
- route fixtures through wp-codebox

## [2.105.9] - 2026-05-30

### Fixed
- retire native core-dev test runner

## [2.105.8] - 2026-05-30

### Fixed
- project wp-codebox apply artifacts into core contract

## [2.105.7] - 2026-05-30

### Fixed
- harden build helper installs

## [2.105.6] - 2026-05-30

### Fixed
- emit WordPress lint producer summaries

## [2.105.5] - 2026-05-30

### Fixed
- keep runner prelude self-contained

## [2.105.4] - 2026-05-30

### Fixed
- use archive install policy for WordPress deploys

## [2.105.3] - 2026-05-30

### Changed
- route fix results through typed sidecars
- use typed sidecar writer APIs

## [2.105.2] - 2026-05-30

### Changed
- Use shared runner prelude in extension runners

## [2.105.1] - 2026-05-30

### Changed
- Normalize extension fix result capture

## [2.105.0] - 2026-05-30

### Added
- consume manifest changed-test routing

## [2.104.3] - 2026-05-30

### Changed
- Add shared command capture helper

## [2.104.2] - 2026-05-30

### Changed
- Add generic settings accessors
- Use core helper for annotation sidecars
- share test result parser adapters

## [2.104.1] - 2026-05-30

### Fixed
- Fix Rust runner step fallback

## [2.104.0] - 2026-05-30

### Added
- expose admin page scenario profiler

### Changed
- Use core sidecar helpers in WordPress runners
- Route Playground validation through WP Codebox
- Fail non-actionable Codebox fanout success
- Stop strict comparison fixer rewrites
- Report Codebox browser memory comparisons
- Parse Codebox browser artifacts into bench metrics

## [2.103.1] - 2026-05-29

### Changed
- Revert "feat: support Codebox agent sandbox bench workloads (#872)"

## [2.103.0] - 2026-05-29

### Added
- support Codebox agent sandbox bench workloads

### Fixed
- resolve runner workspace tools
- pass WP Codebox task timeout into recipes
- avoid metadata-preserving WordPress deploy installs

## [2.102.48] - 2026-05-29

### Fixed
- persist runner workspace aliases

## [2.102.47] - 2026-05-29

### Fixed
- prefer runtime terminal bridge env

## [2.102.46] - 2026-05-29

### Fixed
- use WP Codebox bridge for agent WP-CLI

## [2.102.45] - 2026-05-29

### Fixed
- dispatch runtime WP-CLI compatibility commands

## [2.102.44] - 2026-05-29

### Fixed
- project plugin list inside agent runtime

## [2.102.43] - 2026-05-29

### Fixed
- keep wp eval in agent runtime

## [2.102.42] - 2026-05-29

### Fixed
- run agent WP-CLI tools inside runtime

## [2.102.41] - 2026-05-29

### Fixed
- preserve wp-codebox fanout failure evidence

## [2.102.40] - 2026-05-29

### Fixed
- omit incomplete wp-gym replay projections

## [2.102.39] - 2026-05-29

### Fixed
- keep runner PR publication out of task tools

## [2.102.38] - 2026-05-29

### Fixed
- project wp-gym fields into sealed artifacts

## [2.102.37] - 2026-05-29

### Fixed
- declare WP Codebox seed excludes

## [2.102.36] - 2026-05-29

### Fixed
- tolerate array-shaped eval run metadata

## [2.102.35] - 2026-05-29

### Changed
- Allow partial WP Codebox fanout apply
- Add agent runner evidence artifacts

### Fixed
- avoid pipefail-sensitive terminal tokens

## [2.102.34] - 2026-05-29

### Changed
- Route WP Codebox artifacts by workspace root

## [2.102.33] - 2026-05-29

### Changed
- Seed orchestrator workspaces for WP Codebox audit fanout

## [2.102.32] - 2026-05-29

### Changed
- Report WP Codebox artifact apply failures

## [2.102.31] - 2026-05-29

### Fixed
- attach runner metrics to wp codebox fanout

## [2.102.30] - 2026-05-28

### Fixed
- complete wp codebox no-op outcomes

## [2.102.29] - 2026-05-28

### Fixed
- accept wp codebox artifact outcomes

## [2.102.28] - 2026-05-28

### Fixed
- preserve wp codebox fanout outcomes

## [2.102.27] - 2026-05-28

### Changed
- Install Composer deps for cloned validation dependencies

## [2.102.26] - 2026-05-28

### Fixed
- pass WP Codebox max turns setting

## [2.102.25] - 2026-05-28

### Fixed
- group wp codebox audit remediation

## [2.102.24] - 2026-05-28

### Changed
- Allow no-change agent runs without PR requirement

## [2.102.23] - 2026-05-28

### Changed
- Read nested PR evidence metadata

## [2.102.22] - 2026-05-28

### Changed
- Read nested transcript artifacts in runner evidence

## [2.102.21] - 2026-05-28

### Fixed
- fan out wp codebox audit findings

## [2.102.20] - 2026-05-28

### Changed
- Handle nested transcript artifacts

## [2.102.19] - 2026-05-28

### Fixed
- bound wp codebox fanout tasks

## [2.102.18] - 2026-05-28

### Fixed
- rely on core wp ai client in agent ci

## [2.102.17] - 2026-05-28

### Changed
- Validate live-run wp-gym eval rows

## [2.102.16] - 2026-05-28

### Changed
- Project agent runs to wp-gym eval rows
- Run WP Codebox verifier and policy checks

## [2.102.15] - 2026-05-28

### Fixed
- run wp codebox fanout concurrently

## [2.102.14] - 2026-05-28

### Fixed
- apply wp codebox fanout patches

## [2.102.13] - 2026-05-27

### Changed
- Emit agent run evidence references

## [2.102.12] - 2026-05-27

### Changed
- Skip PHP AI Client as WP Codebox plugin

## [2.102.11] - 2026-05-27

### Fixed
- propagate wp codebox fanout failures

## [2.102.10] - 2026-05-27

### Fixed
- fail wp codebox fanout on task errors

## [2.102.9] - 2026-05-27

### Changed
- share bash version preflight

### Fixed
- align runner step fallback with core

## [2.102.8] - 2026-05-27

### Fixed
- Fix WP Codebox provider plugin main file detection

## [2.102.7] - 2026-05-27

### Fixed
- keep streamed fanout stderr deterministic

## [2.102.6] - 2026-05-27

### Fixed
- format only scoped files

## [2.102.5] - 2026-05-27

### Fixed
- stream WP Codebox audit fanout progress

## [2.102.4] - 2026-05-27

### Fixed
- persist WP Codebox fanout runs incrementally
- keep WP Codebox fanout artifacts external

## [2.102.3] - 2026-05-27

### Fixed
- normalize WP Codebox runner plugin slugs

## [2.102.2] - 2026-05-26

### Fixed
- mount wp-codebox audit repo as workspace

## [2.102.1] - 2026-05-26

### Fixed
- route wp-codebox audit fanout through task runner

## [2.102.0] - 2026-05-26

### Added
- route audit fanout through refactor source

### Changed
- Run fanout tasks through WP Codebox recipes

### Fixed
- accept generic refactor source results

## [2.101.1] - 2026-05-26

### Changed
- Execute audit fanout through WP Codebox

## [2.101.0] - 2026-05-26

### Added
- add WP Codebox audit fanout planner

### Fixed
- tighten audit fanout branch slugs
- harden WP Codebox audit apply-back metadata

## [2.100.9] - 2026-05-24

### Fixed
- handle large WordPress lint sidecars

## [2.100.8] - 2026-05-22

### Fixed
- exempt callback policy adapters from interface audits

## [2.100.7] - 2026-05-22

### Fixed
- filter WordPress ability CLI boilerplate
- ignore placeholder doc references

## [2.100.6] - 2026-05-22

### Fixed
- skip API helper roles in registration audit

## [2.100.5] - 2026-05-22

### Fixed
- reduce ability helper naming noise

## [2.100.4] - 2026-05-22

### Fixed
- make coverage completeness audit advisory
- ignore i18n text domains in slug audit

## [2.100.3] - 2026-05-21

### Fixed
- skip non-php files in php fixers

## [2.100.2] - 2026-05-21

### Fixed
- install wp-codebox from release artifacts

## [2.100.1] - 2026-05-21

### Fixed
- omit optional wp-codebox setup deps

## [2.100.0] - 2026-05-21

### Added
- add scenario verifier hooks

## [2.99.2] - 2026-05-21

### Fixed
- classify wordpress changed-since registration drift

## [2.99.1] - 2026-05-21

### Fixed
- pass multisite mode to wp-codebox tests

## [2.99.0] - 2026-05-21

### Added
- declare ci reproduction profiles

## [2.98.0] - 2026-05-21

### Added
- run core-dev tests through wp-codebox

## [2.97.6] - 2026-05-21

### Fixed
- lock wordpress npm dependencies

## [2.97.5] - 2026-05-21

### Fixed
- avoid activating php ai client in wp-codebox

## [2.97.4] - 2026-05-21

### Fixed
- mount default openai runtime for wp-codebox agents
- map host bundle paths for wp-codebox agents

## [2.97.3] - 2026-05-21

### Fixed
- fail wp-codebox tests on bootstrap errors

## [2.97.2] - 2026-05-21

### Fixed
- accept opened agent PR as success

## [2.97.1] - 2026-05-21

### Fixed
- mount wordpress extension in wp-codebox tests

## [2.97.0] - 2026-05-20

### Added
- expose terminal action workflow input

## [2.96.12] - 2026-05-20

### Fixed
- remove legacy playground setup

## [2.96.11] - 2026-05-20

### Fixed
- stop mounting phpunit runner
- pass phpunit wrapper host path

## [2.96.10] - 2026-05-20

### Fixed
- pass phpunit args to wp-codebox

## [2.96.9] - 2026-05-20

### Fixed
- mount wp-codebox phpunit wrapper

## [2.96.8] - 2026-05-20

### Fixed
- use wp-codebox phpunit recipe command

## [2.96.7] - 2026-05-20

### Fixed
- run phpunit through wp-codebox recipes

## [2.96.6] - 2026-05-20

### Fixed
- normalize wp-codebox bench results

## [2.96.5] - 2026-05-20

### Fixed
- run agents through wp-codebox recipes

## [2.96.4] - 2026-05-20

### Fixed
- pass app token to runner workspace clones

## [2.96.3] - 2026-05-20

### Fixed
- generate wp-codebox bench recipes

## [2.96.2] - 2026-05-20

### Fixed
- remove legacy playground runners

## [2.96.1] - 2026-05-20

### Fixed
- route bench settings through wp-codebox

## [2.96.0] - 2026-05-20

### Added
- run basic benches through wp-codebox

## [2.95.7] - 2026-05-20

### Fixed
- install wp-codebox during setup

## [2.95.6] - 2026-05-20

### Fixed
- default tests to wp-codebox

## [2.95.5] - 2026-05-20

### Fixed
- preserve scoped validation dependencies

## [2.95.4] - 2026-05-20

### Fixed
- report validation dependency provenance

## [2.95.3] - 2026-05-20

### Fixed
- require wp-codebox agent runner

## [2.95.2] - 2026-05-20

### Fixed
- verify deployed artifact paths robustly

## [2.95.1] - 2026-05-20

### Fixed
- suppress empty PHPStan raw output

## [2.95.0] - 2026-05-20

### Added
- add wp-codebox test runner mounts

## [2.94.0] - 2026-05-20

### Added
- parse wp-codebox test artifacts

## [2.93.0] - 2026-05-20

### Added
- add wp-codebox test runtime

## [2.92.0] - 2026-05-20

### Added
- wire wp-codebox agent runtime

## [2.91.0] - 2026-05-19

### Added
- add wp-codebox apply adapter

## [2.90.5] - 2026-05-19

### Fixed
- surface wp-codebox agent artifacts

## [2.90.4] - 2026-05-19

### Fixed
- verify deploy artifacts

## [2.90.3] - 2026-05-19

### Changed
- Cover WP Codebox agent runner adapter

## [2.90.2] - 2026-05-19

### Changed
- Add WP Codebox agent runner adapter

## [2.90.1] - 2026-05-18

### Fixed
- expose artifact PRs in engine data

## [2.90.0] - 2026-05-17

### Added
- mirror release ZIP to a CORS-friendly branch

## [2.89.0] - 2026-05-17

### Added
- add release.package and release.publish actions

## [2.88.1] - 2026-05-16

### Fixed
- accept completion outcome agent runs

## [2.88.0] - 2026-05-16

### Added
- allow Datamachine agent CI without directives

## [2.87.0] - 2026-05-16

### Added
- bridge agent terminal actions into Datamachine
- add host terminal actions

## [2.86.1] - 2026-05-15

### Fixed
- prefer app token for agent GitHub writes

## [2.86.0] - 2026-05-15

### Added
- export non-workspace episode replay JSONL

## [2.85.2] - 2026-05-15

### Fixed
- pass wp cli globals before commands

## [2.85.0] - 2026-05-15

### Added
- seal datamachine eval replay artifacts

## [2.84.1] - 2026-05-14

### Fixed
- define PLUGIN_SLUG early so composer-test fallback works without tests/ dir
- initialize playground plugin slug before composer fallback

## [2.84.0] - 2026-05-14

### Added
- declare content deploy path root

## [2.83.0] - 2026-05-13

### Added
- attribute REST request callers

## [2.82.0] - 2026-05-13

### Added
- suggest REST preload declarations

## [2.81.0] - 2026-05-13

### Added
- expose REST preload checks

## [2.80.0] - 2026-05-13

### Added
- diagnose REST preload misses

## [2.79.0] - 2026-05-13

### Added
- evaluate general rule results

## [2.78.0] - 2026-05-13

### Added
- preserve eval rule metadata

## [2.77.1] - 2026-05-13

### Fixed
- support block theme builds

## [2.77.0] - 2026-05-13

### Added
- add canonical agent eval artifact

## [2.76.0] - 2026-05-13

### Added
- fingerprint agent eval inputs
- support frame readiness functions
- expose phased page readiness

## [2.75.6] - 2026-05-12

### Fixed
- merge child completion outcomes

## [2.75.5] - 2026-05-12

### Changed
- guard read-only lint from phpcbf
- pin PHPStan baseline and level behavior

## [2.75.4] - 2026-05-12

### Fixed
- mark empty PHPUnit discovery as skipped

## [2.75.3] - 2026-05-12

### Fixed
- refresh runner PR summaries after grading

## [2.75.2] - 2026-05-12

### Fixed
- report empty PHPUnit discovery

## [2.75.1] - 2026-05-12

### Fixed
- Fix data-machine dependency resolution for scoped PHPStan

## [2.75.0] - 2026-05-12

### Added
- declare structured sidecar capabilities

## [2.74.2] - 2026-05-12

### Changed
- Template runner workspace fallback PRs

## [2.74.1] - 2026-05-12

### Changed
- centralize shared profiler defaults

## [2.74.0] - 2026-05-12

### Added
- support scoped runner workspace roots

## [2.73.0] - 2026-05-12

### Added
- add admin page sweep summaries

## [2.72.0] - 2026-05-12

### Added
- support opaque runner workspace aliases

## [2.71.1] - 2026-05-12

### Fixed
- consume generic audit detector contracts

## [2.71.0] - 2026-05-12

### Added
- add declarative browser interactions

## [2.70.0] - 2026-05-12

### Added
- add REST budgets and matrix profiling
- add profiling fixture setup hooks

### Fixed
- align budget findings with core schema
- budget full REST payload sizes

## [2.69.1] - 2026-05-12

### Fixed
- import agent before execute workflows

## [2.69.0] - 2026-05-12

### Added
- enrich agent artifact pull requests

## [2.68.1] - 2026-05-12

### Fixed
- make job artifact export opt-in

## [2.68.0] - 2026-05-12

### Added
- run agent execute-workflow payloads

## [2.67.3] - 2026-05-12

### Fixed
- isolate matrix artifact branches
- export agent job artifacts to PRs

## [2.67.2] - 2026-05-12

### Fixed
- preserve repository token for agent artifacts

## [2.67.1] - 2026-05-12

### Fixed
- tune audit convention noise

## [2.67.0] - 2026-05-12

### Added
- capture REST response samples

## [2.66.1] - 2026-05-12

### Fixed
- authenticate bundle repo clones

## [2.66.0] - 2026-05-11

### Added
- add hidden agent workspace capture

## [2.65.1] - 2026-05-11

### Fixed
- drain Data Machine child jobs

## [2.65.0] - 2026-05-11

### Added
- add block theme quality probe

## [2.64.0] - 2026-05-11

### Added
- emit playground bench artifacts
- add playground scenario manifests

## [2.63.0] - 2026-05-11

### Added
- report WordPress REST network diffs

## [2.62.0] - 2026-05-11

### Added
- add data machine agent replay bundles

## [2.61.0] - 2026-05-11

### Added
- define playground grader reward schema
- support custom agent CI provider plugins

## [2.60.6] - 2026-05-11

### Fixed
- stop deriving runner workspace fallback prs

## [2.60.5] - 2026-05-11

### Fixed
- support engine data jq expressions

## [2.60.4] - 2026-05-11

### Fixed
- finalize runner workspace fallback prs

## [2.60.3] - 2026-05-11

### Fixed
- omit empty fallback pr base

## [2.60.2] - 2026-05-11

### Fixed
- report fallback pr errors

## [2.60.1] - 2026-05-11

### Fixed
- collect runner workspace writes

## [2.60.0] - 2026-05-11

### Added
- correlate WordPress timings by browser phase
- include default agent runtime dependencies

## [2.59.0] - 2026-05-11

### Added
- provision runner-owned agent worktrees
- recommend page profiler gates

## [2.58.0] - 2026-05-11

### Added
- report unused REST preload cost

## [2.57.0] - 2026-05-11

### Added
- add wp-admin page scenarios

### Fixed
- allow custom admin page scenarios

## [2.56.0] - 2026-05-11

### Added
- capture REST preload waterfalls

## [2.55.0] - 2026-05-11

### Added
- expose agent CI post-run hooks

## [2.54.0] - 2026-05-11

### Added
- expose agent CI engine data keys

## [2.53.0] - 2026-05-11

### Added
- expose agent CI runner extensions

## [2.52.5] - 2026-05-11

### Fixed
- reuse existing fallback agent pull requests

## [2.52.4] - 2026-05-10

### Added
- diagnose page profile bottlenecks

### Fixed
- attach agent artifacts to real pull requests

## [2.52.3] - 2026-05-10

### Fixed
- detect current-run agent pull requests

## [2.52.2] - 2026-05-10

### Fixed
- allow bundled prompts in agent CI

## [2.52.1] - 2026-05-10

### Fixed
- materialize agent CI transcript content

## [2.52.0] - 2026-05-10

### Added
- add WordPress page profiler helpers

## [2.51.9] - 2026-05-10

### Fixed
- open fallback PR for required agent runs

## [2.51.8] - 2026-05-10

### Fixed
- preserve agent flow prompts in runner

## [2.51.7] - 2026-05-10

### Fixed
- report Data Machine agent workload errors

## [2.51.6] - 2026-05-10

### Fixed
- fail core lint on analyzer failures

## [2.51.5] - 2026-05-10

### Fixed
- allow agent completion outcomes to satisfy CI

## [2.51.4] - 2026-05-10

### Changed
- Make Data Machine agent artifact export configurable

## [2.51.3] - 2026-05-10

### Changed
- Export Data Machine bundle artifacts

## [2.51.2] - 2026-05-10

### Fixed
- classify nested agent tool results

## [2.51.1] - 2026-05-10

### Fixed
- fail agent writes without PRs

## [2.51.0] - 2026-05-10

### Added
- add shared Data Machine agent bundle validator

## [2.50.1] - 2026-05-10

### Fixed
- persist Playground plugin activation state

## [2.50.0] - 2026-05-10

### Added
- add engine data extraction helper
- add generic ci-driver plugin fixture

## [2.49.5] - 2026-05-10

### Fixed
- mount external Data Machine agent bundles

## [2.49.4] - 2026-05-10

### Fixed
- force Data Machine agent tool parameters

## [2.49.3] - 2026-05-10

### Fixed
- support reusable Data Machine agent bundles

## [2.49.2] - 2026-05-10

### Fixed
- wait for Data Machine agent retries

## [2.49.1] - 2026-05-09

### Fixed
- bootstrap Data Machine runner helpers

## [2.49.0] - 2026-05-09

### Added
- add Data Machine runner tool recorders

## [2.48.0] - 2026-05-09

### Added
- add Data Machine runner bootstrap steps

## [2.47.0] - 2026-05-09

### Added
- add generic Data Machine agent runner

## [2.46.0] - 2026-05-08

### Added
- validate Playground blueprints

## [2.45.0] - 2026-05-08

### Added
- mount dependency files in Playground

## [2.44.21] - 2026-05-08

### Fixed
- render playground config defines first

## [2.44.20] - 2026-05-08

### Fixed
- skip dependency drop-ins during playground load

## [2.44.19] - 2026-05-08

### Changed
- name dependency resolver helpers

## [2.44.18] - 2026-05-08

### Fixed
- allow disabling bench warmup

## [2.44.17] - 2026-05-08

### Fixed
- preserve resolved dependency order

## [2.44.16] - 2026-05-08

### Fixed
- load transitive dependencies first

## [2.44.15] - 2026-05-08

### Fixed
- replay ability init after deferred bootstrap
- skip resolved dependency slugs

## [2.44.14] - 2026-05-08

### Fixed
- merge prepared dependency paths

## [2.44.13] - 2026-05-08

### Fixed
- preserve prepared validation deps

## [2.44.12] - 2026-05-08

### Fixed
- defer plugin runtime callbacks during install

## [2.44.11] - 2026-05-08

### Fixed
- preserve playground JSON template payloads

## [2.44.10] - 2026-05-08

### Fixed
- run deferred init after activation

## [2.44.9] - 2026-05-07

### Fixed
- preserve bench env JSON replacements

## [2.44.8] - 2026-05-07

### Fixed
- surface WP-CLI failure output

## [2.44.7] - 2026-05-07

### Fixed
- seed WP-CLI prompt config

## [2.44.6] - 2026-05-07

### Fixed
- exempt == null from eqeqeq rule

## [2.44.5] - 2026-05-07

### Fixed
- use latest Playground CLI

## [2.44.4] - 2026-05-07

### Fixed
- skip wp_filesystem rewrite in test files and fix Elvis-operator expansion

## [2.44.3] - 2026-05-07

### Fixed
- pin Playground CLI dependency

## [2.44.2] - 2026-05-07

### Fixed
- give workload wp-cli steps the bundled WP-CLI command surface

## [2.44.1] - 2026-05-07

### Fixed
- route bench dump_diagnostics to stderr so CI failures surface

## [2.44.0] - 2026-05-07

### Added
- correlate browser resource timings with request profiler

## [2.43.1] - 2026-05-07

### Fixed
- allow Playground login self-redirect readiness

## [2.43.0] - 2026-05-07

### Added
- add Playground HTTP readiness helper

## [2.42.1] - 2026-05-07

### Fixed
- escape sed replacement metacharacters in Playground bench/test runners

## [2.42.0] - 2026-05-07

### Added
- add WordPress request profiler helper

## [2.41.4] - 2026-05-06

### Fixed
- replay deferred init callbacks in hook context

## [2.41.3] - 2026-05-06

### Fixed
- configure playground core version

## [2.41.2] - 2026-05-06

### Fixed
- load blueprint plugins for bench workloads

## [2.41.1] - 2026-05-06

### Fixed
- dedupe playground dependency mounts

## [2.41.0] - 2026-05-06

### Added
- defer plugin activation until after wp-phpunit install (#431)
- declare lockfile_paths for autofix drift resolution
- configurable Playground bench workloads
- add post-write validate and format scripts

### Fixed
- declare WP-CLI command output recognizers
- forward playground phpunit args
- tighten option-scope-drift detector for single-site plugins
- anchor constant-backed slug detector to real class declarations (#425)
- load bench deps before plugins_loaded fires (#426)
- refresh stale frontend dependencies
- split auth and policy audit roles
- split artifact audit role
- rename wp-only fixture so role-tag splitting keeps it scanned
- split audit roles + curate dead-guard known symbols
- tune audit detector context
- skip eslint when no js files
- exclude prefixed vendor dirs from lint

## [2.39.2] - 2026-05-01

### Fixed
- add wordpress-stubs to scanFiles so host-smoke shims cannot shadow real WP signatures
- resolve transitive validation dependencies

## [2.39.1] - 2026-05-01

### Fixed
- split lint profiles by file role

## [2.39.0] - 2026-05-01

### Added
- normalize test failure sidecar

## [2.38.0] - 2026-05-01

### Added
- enrich lint findings sidecar

## [2.37.1] - 2026-05-01

### Fixed
- run composer tests when PHPUnit discovery is empty

## [2.37.0] - 2026-04-30

### Added
- add fingerprint metadata to grammars

## [2.36.0] - 2026-04-30

### Added
- declare manifest routing metadata

## [2.35.0] - 2026-04-30

### Added
- own requested audit detectors

## [2.34.2] - 2026-04-30

### Fixed
- tighten PHPStan API stubs

## [2.34.1] - 2026-04-30

### Fixed
- exclude non-runtime files from full PHPStan lint

## [2.34.0] - 2026-04-30

### Added
- emit browser bench target metadata

## [2.33.0] - 2026-04-29

### Added
- declare deploy safety policy

### Fixed
- emit runtime dispatch metadata

## [2.32.0] - 2026-04-29

### Added
- provide dead-guard known symbols

## [2.31.0] - 2026-04-29

### Added
- add trace runner support

## [2.30.15] - 2026-04-29

### Fixed
- route changed smoke files to host backend

## [2.30.14] - 2026-04-29

### Fixed
- log playground install diagnostics
- defer install-time plugin callbacks

## [2.30.13] - 2026-04-29

### Fixed
- load playground test plugins during bootstrap

## [2.30.12] - 2026-04-29

### Changed
- cover scoped eslint file filtering

## [2.30.11] - 2026-04-29

### Fixed
- route standalone smoke files

## [2.30.10] - 2026-04-29

### Fixed
- prepare Playground test schema

## [2.30.9] - 2026-04-29

### Fixed
- forward PHPUnit args in Playground tests

## [2.30.8] - 2026-04-29

### Fixed
- skip non-js scoped eslint files

## [2.30.7] - 2026-04-29

### Fixed
- handle absolute PHPStan scope paths
- preserve PHPStan context for scoped lint

## [2.30.6] - 2026-04-28

### Fixed
- respect scoped lint and test files

## [2.30.5] - 2026-04-28

### Fixed
- load runtime context for scoped PHPStan
- scope lint profile to runtime files

## [2.30.4] - 2026-04-28

### Fixed
- support host smoke test backend

## [2.30.3] - 2026-04-28

### Fixed
- classify empty PHPUnit discovery
- quiet stale Playground temp ENOENT noise
- preserve PHPStan temp config suffixes

## [2.30.2] - 2026-04-27

### Fixed
- remove duplicate test config

## [2.30.1] - 2026-04-27

### Fixed
- resolve Playground extension mount paths

## [2.30.0] - 2026-04-27

### Added
- declare drift selection config

## [2.29.0] - 2026-04-27

### Added
- declare cli auto flags

## [2.28.0] - 2026-04-27

### Added
- add component env detector

## [2.27.2] - 2026-04-27

### Changed
- use shared Homeboy helpers

## [2.27.1] - 2026-04-27

### Fixed
- adopt shared resolve context

## [2.27.0] - 2026-04-27

### Added
- declare remote path inference rules

## [2.26.1] - 2026-04-27

### Fixed
- seed lint runner plugin path

## [2.26.0] - 2026-04-27

### Added
- support persisted bench site scenarios

## [2.25.0] - 2026-04-27

### Added
- detect mock-over-fixture test smells

## [2.24.0] - 2026-04-27

### Added
- support core-dev component shape

## [2.23.2] - 2026-04-27

### Fixed
- fail lint runner on reported issues

## [2.23.1] - 2026-04-27

### Fixed
- lock extension deps to supported PHP floor

## [2.23.0] - 2026-04-27

### Added
- preserve bench workload metrics

## [2.22.0] - 2026-04-27

### Added
- support list-only scenario discovery

## [2.21.0] - 2026-04-27

### Added
- filter bench workloads

## [2.20.2] - 2026-04-26

### Fixed
- scope PHPStan lint targets

## [2.20.1] - 2026-04-26

### Fixed
- keep extension test paths out of component discovery

## [2.20.0] - 2026-04-26

### Added
- run rig-declared extra workloads

## [2.19.0] - 2026-04-26

### Added
- feat(wordpress-bench): surface bootstrap stage timings as __bootstrap scenario

## [2.18.1] - 2026-04-26

### Fixed
- repair lint runner tool resolution

## [2.18.0] - 2026-04-25

### Added
- bench_env passthrough for host-shell vars into Playground

## [2.17.1] - 2026-04-25

### Fixed
- plugin slug honors HOMEBOY_COMPONENT_ID instead of basename

## [2.17.0] - 2026-04-25

### Added
- wp_config_defines setting for per-component wp-config additions (#247)

## [2.16.1] - 2026-04-25

### Fixed
- use BSD-compatible mktemp templates

## [2.16.0] - 2026-04-25

### Added
- bench shared-state + concurrency contract

## [2.15.0] - 2026-04-25

### Added
- multi-line comment style sniff (closes #239)

## [2.14.0] - 2026-04-25

### Added
- add bench capability dispatcher (Phase 4 of #214)

## [2.13.1] - 2026-04-25

### Changed
- extract Playground bootstrap helpers into shared lib

## [2.13.0] - 2026-04-24

### Added
- PHPStan level 7 + include tests + fix baseline integration

## [2.12.0] - 2026-04-24

### Added
- surface auto-fixable lint findings as prominent CTA

## [2.11.0] - 2026-04-23

### Added
- retire host-PHP test backend — Playground only (Phase 3 of #214)

## [2.10.18] - 2026-04-23

### Changed
- replace wp plugin/theme install with pure unzip + atomic rename

## [2.10.17] - 2026-04-23

### Changed
- playground db.php drop-in coexistence — fixture + smoke test

## [2.10.16] - 2026-04-23

### Changed
- playground test discovery — recursive + phpunit.xml.dist-aware

## [2.10.15] - 2026-04-23

### Changed
- playground diagnostics — structured stage logging + classified failures

## [2.10.14] - 2026-04-23

### Changed
- add Playground as opt-in test backend (Phase 1 of #214)

## [2.10.13] - 2026-04-20

### Fixed
- extend hook_callbacks coverage to all WP registration patterns (homeboy#1149)

## [2.10.12] - 2026-04-19

### Fixed
- correct database default docs (#206) + bypass PHPStan parallel worker crash (#207)

## [2.10.11] - 2026-04-18

### Fixed
- make HOMEBOY_FIX_ONLY the single auto-fix contract for all runners (#1145)

## [2.10.10] - 2026-04-18

### Fixed
- allow leading whitespace on top-level namespace/class/use (#1134)

## [2.10.9] - 2026-04-18

### Fixed
- macOS portability — replace grep -P with sed, add bash 4+ gate to test-runner (#1144, #1146)

## [2.10.8] - 2026-04-09

### Changed
- Add FS_CHMOD and FS_METHOD constants to test wp-tests-config

## [2.10.7] - 2026-04-07

### Fixed
- load plugins at muplugins_loaded instead of plugins_loaded in test bootstrap

## [2.10.6] - 2026-04-04

### Fixed
- PSR-4 aware trait placement + body comparison guard

## [2.10.5] - 2026-04-02

### Fixed
- enable post-deploy permission fix for WordPress plugins and themes

## [2.10.4] - 2026-03-23

### Fixed
- atomic swap deploy with backup-before-replace for plugins and themes

## [2.10.3] - 2026-03-23

### Fixed
- strengthen autofixer semantic safety and framework awareness

## [2.10.2] - 2026-03-22

### Fixed
- use .homeboy-build/ staging dir to avoid build/ collision with npm output (fixes #169)
- auto-detect PHP version from Requires PHP header for PHPStan (fixes #178)

## [2.10.1] - 2026-03-21

### Fixed
- rename extracted zip folder to match remote_path basename (fixes #176)

## [2.10.0] - 2026-03-21

### Added
- independent component versioning and continuous release for each extension
- add field_assertion_template for Rust and PHP
- add field_pattern for struct/class property extraction (#818)
- add type_constructors and assertion_templates for Rust and PHP (#818)
- add php -l syntax validation gate for post-write safety
- write lint findings sidecar for categorized issues
- add [contract] grammar for PHP test generation
- add call_sites to fingerprint output (#824)
- audit reference dependencies — resolve WP core + plugin deps
- add git-clone fallback for CI dependency resolution
- auto-discover dependencies from Requires Plugins header
- emit fix plan sidecars for lint and test
- handle HOMEBOY_CHANGED_TEST_FILES for scoped test runs
- write test infrastructure fix results to HOMEBOY_FIX_RESULTS_FILE sidecar
- write structured fix results to HOMEBOY_FIX_RESULTS_FILE sidecar
- emit type_names in fingerprint output
- add crossref extension script for test/production hook analysis
- WP alternatives + WP Filesystem fixers
- add 4 PHPCS auto-fixers for silenced errors, empty catches, readdir loops, and commented code
- add WordPress-aware unused parameter fixer
- add 4 new auto-fixers for PHPCS violations
- support generic npm build scripts
- language grammar files for structural regex engine
- test output parsers for baseline ratchet and failure analysis
- write test results JSON for homeboy test baseline
- add code coverage collection to test runners
- annotation sidecar JSON for CI inline review
- add test_mapping config to Rust and WordPress extensions
- add extends, visibility, properties, hooks to fingerprint
- auto-detect common namespace prefix and tab-aware indentation
- add refactor script with parse_items + extract_shared
- add method_hashes and structural_hashes to PHP fingerprint
- Add fingerprint scripts for Rust and WordPress extensions
- add post:deploy hooks for plugin activation and cache flush
- add since_tag config for @since placeholder replacement
- support HOMEBOY_STEP/HOMEBOY_SKIP for step filtering
- add cleanup_paths to wordpress, nodejs, and rust modules
- granular audit_feature_patterns for WordPress module
- add audit_feature_patterns for undocumented feature detection
- detect and warn about conflicting local test infrastructure
- add PHPStan static analysis integration
- add lint/test config and reorganize scripts
- add autoload error detection before PHPUnit
- add ESLint runner, Swift module, and documentation
- auto-detect text domain from plugin header
- add polyfills dependency, local file detection, build overrides
- add PHPUnit test infrastructure with SQLite support
- add deploy verification, version patterns, and build config

### Changed
- extract bridge framework, delete stale runtime helpers
- wordpress v2.9.0
- add parallel processing to PHPCS/PHPCBF in lint runner
- extract shared helpers for context resolution and component detection
- v2.9.1
- v2.7.0
- share runner step filtering
- split PHPUnit failure parsing into modular parsers
- v2.6.0
- resolve conflicts with main (PR #82 results parsers)
- Add dead code fields to Rust and WordPress fingerprint scripts
- v2.2.1
- Add feature_context rules and richer templates to Rust and WordPress extensions
- Add audit doc config (feature_labels, doc_targets) to WordPress and Rust extensions
- rename modules to extensions across repo
- v2.1.0
- migrate all module manifests to grouped capability structure
- Refactor script organization and update WordPress module configuration
- rebuilt wordpress testing structure for theme support... modularized for scalability and maintainability
- pre-release changes
- finally fixed ongoing blank return 126 code from overly large output
- updated claude rule
- agent tools module added
- Extract shared fixer helper with directory exclusions
- added more automated lint fixers
- Add PHPCS/PHPCBF vendor packages and test infrastructure
- Add in_array strict and short ternary fixers, enhance Yoda fixer
- Add Yoda condition fixer and enhanced lint runner
- added universal build script for wordpress projects.
- added deploy override to wordpress module for reliability during deployment time
- convert module configs to snake_case keys
- Initial commit

### Fixed
- run plugin entry-file load in subprocess to survive fatals
- include PHPStan findings in lint baseline ratchet
- make frontend build non-fatal for PHP-primary plugins
- seed changelogs with current versions for homeboy release
- add skip_test_patterns — exclude JS/JSX/CSS and admin UI from PHP test mapping
- add hook callbacks to internal_calls in fingerprinter
- deploy safety and PHP version preflight
- improve lint safety — PHPStan-safe noops, param rename guardrails, critical-only checks, text domain fixer
- load local WordPress validation dependencies
- add testdox fallback parser for crashed PHPUnit runs
- smart PHPUnit exit code handling when all tests pass
- run reserved-param-fixer on full plugin path for cross-file safety
- reduce audit false positives in PHP fingerprint extraction
- update PHP 8 named argument call sites when renaming reserved params
- auto-detect MySQL credentials from wp-config.php
- skip comment lines in WP filesystem fixer
- prevent WP filesystem fixer from re-replacing already-fixed calls
- empty-catch fixer uses unset() + auto-detect PHP version from composer.json
- expand yoda and short-ternary fixers to cover real-world patterns
- reinstall npm deps when expected local bin is missing
- fix(wordpress-tests): align mysql default database with CI service
- fix(wordpress-tests): default MySQL host to TCP for CI reliability
- avoid replaying full PHPUnit output on failure
- database_type default should be 'auto' not 'sqlite'
- add REGEXP user function for MySQL compatibility in SQLite
- add query filter + SQL_CALC_FOUND_ROWS emulation to SQLite driver
- SQLite driver — depth-counting CREATE TABLE parser + ON DUPLICATE KEY translation
- SQLite driver — strip ON UPDATE CURRENT_TIMESTAMP, implement DESCRIBE/SHOW TABLES/SHOW INDEX
- SQLite test backend + bundle WP-CLI for test environment
- downgrade doctrine/instantiator for PHP 8.2 compat
- replace --threads CLI flag with neon config for PHPStan 2.x
- remove auto-activate from post:deploy hook
- SQLite DDL translation, db.php cleanup, and zero-test detection
- prevent wp_not_installed() from killing PHPUnit process
- include css, ts, js, json in WordPress file_extensions
- rename HOMEBOY_MODULE_PATH env var to HOMEBOY_EXTENSION_PATH in build scripts
- use HOMEBOY_COMPONENT_ID for build artifact naming (#227)
- Fix PHPUnit test discovery — drop broken XML config
- auto-detect MySQL for test runner, fix SQLite driver
- stream PHPUnit output instead of capturing into variable
- handle permission-denied when cleaning stale build artifacts
- restore PHPUnit test output visibility
- auto-detect CPU count and retry single-threaded on PHPStan parallel worker failure
- resolve minimatch ReDoS vulnerability via npm override
- check for pdo_sqlite extension before running SQLite-backed tests
- remove unconditional bootstrapFiles from phpstan.neon.dist (fixes #27)
- bump PHPStan memory limit from 1G to 2G (fixes #25)
- fix ESLint path, PHPUnit output buffering, and MySQL config params
- skip tests by default during builds
- download WordPress on demand instead of bundling
- update test-runner.sh path in build script
- remove /src from default excludes and add PSR-4 validation
- add missing WordPress function stubs to validate-autoload.php
- add failure tracking and summary to build output
- add missing WordPress stubs for autoload validation
- standardize lint behavior and add theme support to autoload validation
- run eslint from plugin directory for correct import resolution
- add WordPress function stubs to autoload validation
- lint improvements and bash 4+ compatibility check
- fixing automated builds
- fixed wordpress build script to handle nested blocks, also updated modules providing cli tools to support direct execution
- use {} instead of {{}} in verify_command for find -exec

## [2.9.0] - 2026-03-21

### Added
- Initial independent component release
