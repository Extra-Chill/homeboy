# Homeboy Core Kernel Investigation

Issue: [#13126](https://github.com/Extra-Chill/homeboy/issues/13126)

Measured at `investigate/13126-homeboy-core-kernel` based on `c56e9f889` plus this
report. The investigation treats `homeboy-core` as the kernel. It does not propose
another kernel crate.

## Decision

`homeboy-core` is the minimal generic kernel for a durable automation execution:

- resolve declared targets, workspaces, source identities, and artifact locations;
- select and execute a declared local or remote placement;
- own generic lifecycle identity, durable run/artifact/evidence persistence, and
  recovery of those records;
- define extension discovery, readiness, invocation, and typed provider dispatch;
- enforce generic policy: secrets/redaction, resource limits, cancellation,
  confirmation, output envelopes, and compatibility checks; and
- expose versioned, ecosystem-neutral contracts required on both sides of a
  controller/runner or extension handoff.

`homeboy-core` must not own a command family, an ecosystem grammar, a specific
provider implementation, a release/deploy policy, or a capability-specific
persistence projection. `homeboy-cli` is the composition root: it selects command
packages and registers their adapters. Contract crates below core are appropriate
only when they carry a versioned, cross-boundary vocabulary. A compatibility
re-export is migration evidence, not permanent kernel API.

Target direction:

```text
contracts + primitives
        ^
homeboy-core (generic execution, persistence, extension dispatch)
        ^
capability crates (`homeboy-agents`, runner, audit, fuzz, release, ...)
        ^
homeboy-cli command composition + installed extension commands
        ^
homeboy binary
```

No arrow points from `homeboy-core` to a capability implementation. A capability
may depend on a contract crate, core, and another explicitly required capability;
it must not make every unrelated capability part of the default binary.

## Current Measurements

Bounded local measurements used `cargo metadata --no-deps`, `cargo tree`, and Git
tracked source counts. They do not create a second Cargo target directory.

| Measure | Result | Evidence |
| --- | ---: | --- |
| Workspace packages | 38 | `cargo metadata --no-deps` |
| Rust source lines | 1,178,392 | `git ls-files '*.rs' | xargs wc -l` |
| `homeboy-core` Rust files | 326 | tracked source count |
| `homeboy-cli` Rust files | 356 | tracked source count |
| `homeboy-agents` Rust files | 229 | tracked source count |
| Core direct local production dependencies | 20 | `crates/homeboy-core/Cargo.toml:26-45` |
| CLI direct local production dependencies | 23 | `crates/homeboy-cli/Cargo.toml:23-45` |
| CLI direct optional/domain implementations | 15 | `homeboy-cli/Cargo.toml:28-42` |
| Static top-level CLI commands | 43 | `homeboy-cli/src/cli_surface/mod.rs:165-252` |
| Existing core public modules | 123 at the prior verified inventory | [#11143](https://github.com/Extra-Chill/homeboy/issues/11143) |

The root binary depends on `homeboy-cli` and `homeboy-core`
(`Cargo.toml:58-76`). Therefore every `homeboy-cli` production dependency is in
the default product graph. `cargo tree -p homeboy-cli --edges normal --depth 1`
confirms all 15 domain implementations are direct edges, including Agents, Lab
Runner, Fuzz, Issues, Triage, Tunnel, Refactor, Rig, Release, and Deploy.

The expected graph impact is structural and measurable before timing claims:

| Change | Default direct local edges | Default source/test graph | Expected effect |
| --- | --- | --- | --- |
| Current CLI composition | 23 | all command crates compile for normal binary/test builds | no capability isolation |
| Dynamic optional command packages for Fuzz, Issues/Triage, and Tunnel | 19 or fewer in the kernel CLI package | those packages and their tests leave a kernel-only build | fewer Rust units and fewer command registrations; exact time/size measured by the implementation PR with one shared target |
| Remove audit/refactor from core | core loses 1 domain implementation edge and 2 domain contract edges | code-audit no longer compiles for a kernel-only consumer | removes the current upward domain edge, not merely source relocation |
| Move only one-consumer modules inside CLI | unchanged | unchanged | namespace cleanup only; no graph benefit |

Clean/warm duration and binary-size deltas are intentionally not asserted here.
The worktree has no existing `target/` directory, and creating a clean target just
to benchmark a documentation investigation would duplicate a large build. The
implementation issues must record clean and warm timings using the managed shared
target, `cargo build -p homeboy`, `cargo test -p homeboy-cli`, artifact size, and
the same commit before/after.

Current CI is not a substitute for this measurement. Recent main Release workflows
were repeatedly cancelled; the latest completed required-gate and release-integrity
workflows succeeded, but do not report a capability-isolated build delta.

## Violations

| Violation | Exact evidence | Required correction |
| --- | --- | --- |
| Core imports an Audit implementation | `homeboy-core/Cargo.toml:37-39` imports `homeboy-audit-contract` and `homeboy-code-audit`; `core/src/lib.rs:75-78` re-exports it | move Audit adapters/projections above core; retain generic findings/artifacts only |
| Core owns Audit-specific observation adapters | `core/src/observation/audit_artifact_provider.rs:3-25`, `observation/finding_records.rs:5-202` | Audit owns the conversion/provider and writes generic finding records through a core port |
| Core imports Refactor and Release domain contracts | `homeboy-core/Cargo.toml:36-39`; `refactor_transform_provider.rs:14`; `release_provider.rs:14`; `context/report.rs:14-755` | move capability adapters outward; keep a generic capability-provider port only if multiple independent consumers need it |
| Static CLI links every capability | `homeboy-cli/Cargo.toml:28-42` | optional capability packages register typed command descriptors/handlers; selected packages alone link into a given product build |
| CLI re-exports are mistaken for an architecture boundary | `homeboy-cli/src/lib.rs:10-31`; `homeboy-core/src/lib.rs:49-219` | delete aliases after callers migrate; retain only a documented compatibility window where external source consumers prove a need |
| Runtime registration is a hand-maintained all-capability list | `cli_runtime.rs:159-228` registers Tunnel, Audit, Rig, Stack, Refactor, and Runner adapters directly | registration follows installed/compiled capability descriptors and has a completeness test |
| Generic core has ecosystem-specific code | [#6855](https://github.com/Extra-Chill/homeboy/issues/6855) documents 275 true-positive core-boundary findings; [#11350](https://github.com/Extra-Chill/homeboy/issues/11350) covers Playwright | extensions supply grammars, readiness probes, and remediation metadata |

## Capability Decisions

| Capability | Decision | Boundary and compatibility evidence |
| --- | --- | --- |
| Agent Task | Retain generic contracts and dispatch/lifecycle ports in core; keep `homeboy-agents` controller and capability implementation above core | extensions consume `homeboy/agent-task-*/v1` schemas in `homeboy-extensions/agent-runtimes/fixtures/homeboy-agent-task-core-contract.json:1-84`; no shell-only replacement is acceptable |
| Lab Runner | Retain placement, job handoff, and evidence ports; runner implementation remains a capability | CLI route and runner commands use `homeboy-lab-runner-contract` (`commands/infra/route.rs:653-3326`); runner adapters already register through seams at `cli_runtime.rs:210-227` |
| Audit / Refactor | Extract adapters and language/domain implementations outward | core directly imports Audit; its provider registrations are explicit at `cli_runtime.rs:170-209`; existing [#2240](https://github.com/Extra-Chill/homeboy/issues/2240) and [#6855](https://github.com/Extra-Chill/homeboy/issues/6855) track ecosystem removal |
| Rig | Retain generic materialization contract; no package extraction now | Runner, Lab, and agent-task consume rig data. `cli_runtime.rs:197-200` shows a provider seam, but a separate package would not reduce the default graph until CLI composition changes |
| Fuzz | Accepted first optional command package | extension manifests supply `homeboy/fuzz-campaign/v1` (`homeboy-extensions/nodejs/nodejs.json:91-98`); retain the generic artifact/evidence contract; reuse [#6766](https://github.com/Extra-Chill/homeboy/issues/6766) for schema cleanup |
| Issues / Triage | Accepted as one optional automation package | `homeboy-triage` is directly linked only by CLI; persisted `triage_items` records are owned by the generic observation store (`observation/store/schema.rs:122-151`) and need a compatibility reader before package removal |
| Tunnel | Accepted optional operations package | direct `homeboy_tunnel::register()` at `cli_runtime.rs:169`; its generated reference exposes service and preview commands (`docs/reference/cli/commands/tunnel.md:15-29`); no sibling repository consumer was found in the inspected Extensions checkout |
| Release / Deploy | Retain typed contracts and first-class command packages; no optionalization now | Extensions explicitly rely on `homeboy release` and `homeboy deploy` (`homeboy-extensions/README.md:54-60`, Cloudflare Workers README); Homeboy release automation also invokes `homeboy release` in `.github/workflows/release.yml` |
| Remote operations | No extraction decision | [#8015](https://github.com/Extra-Chill/homeboy/issues/8015) completed with a no-premature-consolidation direction. Keep target/auth/transport primitives in core; do not create an operations layer until it deletes duplicated behavior |
| Runs / evidence | Retain in core | Runs, artifacts, schema migrations, and recovery are kernel durability concerns. `observation/store/schema.rs:17-212` owns the shared SQLite records and indexes |
| Extension dispatch | Retain in core | runtime extension discovery augments the command surface (`cli_runtime.rs:79-102`, `130-147`); extension manifests own ecosystem details (`homeboy-extensions/README.md:20-35`) |

## Consumer And Schema Inventory

The inspected read-only sibling consumer is `homeboy-extensions`. It has concrete
agent-runtime request/outcome, readiness, artifact, and fuzz-manifest consumers;
it also documents Release/Deploy as product behavior. This proves those typed
schemas and command spellings require a migration window. It did not provide a
Tunnel, Issues, or Triage API consumer. Repository-local consumers include the
generated CLI reference, command tests, and the Homeboy release workflow.

Persisted compatibility is more important than Rust paths:

- `runs`, `artifacts`, trace, findings, triage, and publication intent tables are
  migrated by core (`observation/store/schema.rs:17-212`), so capability extraction
  must preserve core read access and migration ownership.
- Agent-task retry identity is indexed from `runs.metadata_json`
  (`schema.rs:190-196`), so mixed old/new writers need identical JSON shape.
- Controller runtime pins are explicitly migrated to
  `homeboy/controller-runtime-pin/v2` (`controller_runtime.rs:1138-1208`).
- Extension-facing Agent Task schemas are v1 strings, not private Rust types.
  New capability packages must accept the existing schema versions before any
  producer changes.

Mixed-version rollout rule: land contract readers first, then dual-register old
and new command/provider paths, then switch writers, then remove a compatibility
re-export only after extension fixtures, persisted-record readers, workflow calls,
and command JSON output have compatibility proof. Never couple a schema migration
to a package-location move without rollback-compatible readers.

## Ordered Sequence

1. Add descriptor-driven capability registration and a test that fails when a
   compiled capability has no registration. This is the prerequisite for deletion.
2. Move Audit/Refactor adapters and ecosystem grammar knowledge above core through
   the existing #2240/#6855 tracks; remove core's direct Audit edges.
3. Establish optional command-package composition for Fuzz, Issues/Triage, and
   Tunnel while preserving their command/result schemas and generated help.
4. Measure the default graph, warm/clean build, focused tests, and binary size
   against the same shared target before claiming a reduction.
5. Migrate proven external Rust callers off `homeboy-core`/`homeboy-cli`
   re-exports; delete aliases rather than preserving a second permanent API.
6. Relocate CLI-only core modules under their owner only when it enables a deleted
   dependency or public API. Otherwise leave them unchanged: pure reshuffling has
   no kernel outcome.
7. Re-evaluate Rig and remote operations after optional composition exists. Keep
   Release/Deploy and Runs/Evidence unchanged unless new consumer evidence shows a
   graph reduction without breaking operations.

## Existing And New Trackers

- Reused: [#2240](https://github.com/Extra-Chill/homeboy/issues/2240),
  [#6855](https://github.com/Extra-Chill/homeboy/issues/6855),
  [#6766](https://github.com/Extra-Chill/homeboy/issues/6766),
  [#8010](https://github.com/Extra-Chill/homeboy/issues/8010),
  [#8015](https://github.com/Extra-Chill/homeboy/issues/8015), and
  [#11143](https://github.com/Extra-Chill/homeboy/issues/11143).
- New: [#13141: Optionalize Fuzz, Issues/Triage, and Tunnel through
  descriptor-driven CLI composition](https://github.com/Extra-Chill/homeboy/issues/13141).

## Residual Risks

- Cargo feature flags alone do not create installable capability packages or a
  stable dynamic command ABI. The implementation must choose a typed package and
  registration mechanism before deleting static edges.
- `homeboy-cli` currently owns a large typed `Commands` enum. Dynamic commands
  require explicit argument/result contracts and command-surface generation; they
  cannot be hidden behind an untyped subprocess convention.
- Core SQLite tables include capability-specific projections. Moving writers must
  retain their core migration/read ownership until an independently versioned
  persistence boundary exists.
- The sibling scan was intentionally read-only and limited to the available
  `homeboy-extensions` checkout plus repository workflows. Each implementation PR
  must repeat a targeted organization-wide consumer search before deleting a
  command spelling or re-export.

## AI Assistance

OpenAI GPT-5.6 Sol via OpenCode inspected the source graph, Cargo metadata,
runtime registration, persisted schemas, CI evidence, and the available
Homeboy Extensions consumer checkout, then drafted this architecture report.
Chris Huber remains responsible for the proposal and all subsequent changes.
