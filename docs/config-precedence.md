# Configuration Precedence Map

Investigation for Extra-Chill/homeboy#7519. This document maps configuration concepts that appear in two or more schemas and records the current runtime precedence from the resolving code. Precedence here means the effective value used by Homeboy after all participating schemas have been loaded.

> **Status.** This is a point-in-time investigation, not a maintained contract. It cites files rather than line ranges: the original `path.rs:LINE` citations all drifted and several files moved during crate extraction and later consolidation (component model and config to `crates/contracts/homeboy-component-contract/`, extension code back into `crates/homeboy-core/src/extension/`, deploy path roots to `crates/homeboy-release/`, rig spec to `crates/homeboy-rig/`, runner loading to `crates/homeboy-lab-runner/`, tunnel entity to `crates/homeboy-tunnel/`). Only the consolidation in "Consolidation Shipped In This Slice" shipped; none of the six removals in "Simplification Proposal" have been implemented, and the five "Ambiguous Or Order-Dependent Areas" remain untraced. Treat both sections as an open backlog, and re-verify against the tree before relying on any claim here.

## Source Schemas

| Schema | Primary source |
| --- | --- |
| Global config (`homeboy.json`) | `HomeboyConfig` and its fields in `crates/homeboy-core/src/defaults.rs` |
| Project config | `Project` in `crates/homeboy-core/src/project/mod.rs` |
| Component registry and repo-local portable config | `Component` in `crates/contracts/homeboy-component-contract/src/model.rs`; portable read/write in `crates/homeboy-core/src/component/portable.rs` |
| Rig spec | `RigSpec` and rig component fields in `crates/homeboy-rig/src/spec.rs` |
| Extension manifest | `ExtensionManifest` in `crates/homeboy-core/src/extension/manifest.rs` |
| Fleet / server / runner / tunnel registry | `ConfigEntity` registry in `crates/homeboy-core/src/config.rs`; entity implementations in `crates/homeboy-core/src/fleet/mod.rs`, `crates/homeboy-core/src/server/mod.rs`, `crates/homeboy-lab-runner/src/lib.rs`, and `crates/homeboy-tunnel/src/entity.rs` |

## Overlapping Concepts

### Component Source Path

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component registry `local_path`; repo-local `homeboy.json` discovered from `--path`, positional directory, or CWD; project component attachment `local_path`; rig component `path`; rig component `path_setting`; runner/workspace paths in runner/server config. | Command target resolution: project-scoped component ID wins first when a project is supplied; explicit `--path` then overrides `local_path`; bare directory comes next; CWD/portable checkout for the requested component is preferred before registry lookup; registry lookup follows; CWD discovery is final fallback. Project-attached components replace the resolved `local_path` with the attachment path after applying overrides. Rig component path resolution is a separate path, not merged with component registry unless the rig omits `path` and uses `component_id` / `path_setting`. | `TargetSpec` documents the shared contract in `crates/homeboy-core/src/component/resolution.rs`. `resolve_target()` documents and implements the command-facing order in `crates/homeboy-core/src/component/resolution.rs` and calls `resolve_effective_inner()` in `crates/homeboy-core/src/component/resolution.rs`. Project attachment normalization overwrites the component path in `crates/homeboy-core/src/project/component/resolution.rs`. Rig component path fields are declared in `crates/homeboy-rig/src/spec.rs`. |

### Component Identity And Aliases

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component registry file stem / `id`; repo-local portable `id`; project component attachment `id`; rig component map key plus optional `component_id`; config entity aliases across project/server/tunnel/component-like registries. | For entity files, the file path ID is authoritative after deserialize because `config::load()` sets the ID from the lookup key. For portable discovery, the repo-local `homeboy.json` must declare a non-empty `id`, which is slugified. For rig components, `component_id` is the registry fallback; when omitted, the map key is the implied component ID. Alias resolution is case-insensitive and happens only when direct entity path lookup misses. | Entity load sets ID and calls `post_load()` in `crates/homeboy-core/src/config.rs`. Alias fallback is implemented in `crates/homeboy-core/src/config.rs`. Portable ID validation and slugification are in `crates/homeboy-core/src/component/portable.rs`. Rig `component_id` semantics are declared in `crates/homeboy-rig/src/spec.rs`. |

### Deploy Target Path

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `remote_path` in registry/portable config; project component attachment `remote_path`; project `component_overrides.<id>.remote_path`; fleet `component_overrides.<id>.remote_path`; extension manifest `deploy.remote_path_inference`; project `path_roots`; extension manifest `deploy.path_roots`; project `base_path`; global deploy defaults for SSH port/artifact prefix/scp flags. | For project-attached components: portable component value loads first; project attachment `remote_path` overrides if non-empty; standalone registry `remote_path` is only a fallback when portable `remote_path` is empty; fleet override is applied next; project override is applied last and wins explicit deploy fields. If the resulting `remote_path` is still empty, extension remote-path inference may fill it. During deploy path expansion, absolute paths are joined safely, managed path roots use project `path_roots`, missing roots can be detected from extension `path_roots.detect_command`, and otherwise the project/base fallback is used or rejected for unsafe `..`. | Attachment override and standalone fallback are in `crates/homeboy-core/src/project/component/resolution.rs`; standalone fallback fields are limited in `crates/homeboy-core/src/project/component/resolution.rs`. Fleet then project override order is implemented in `crates/homeboy-core/src/project/component/overrides.rs`. Extension inference is called after layers in `crates/homeboy-core/src/project/component/resolution.rs` and implemented in `crates/contracts/homeboy-component-contract/src/model.rs`. Deploy path-root resolution is implemented in `crates/homeboy-deploy/src/path_roots.rs`, detection in `crates/homeboy-deploy/src/path_roots.rs`, project-root matching in `crates/homeboy-deploy/src/path_roots.rs`, and extension path-root collection in `crates/homeboy-deploy/src/path_roots.rs`. Global deploy defaults live in `crates/homeboy-core/src/defaults.rs`. |

### Deploy Field Overrides

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `build_artifact`, `extract_command`, `remote_owner`, `deploy_strategy`, `git_deploy`, `artifact_inputs`, `cli_path`, `hooks`; project component overrides for the same fields; fleet component overrides for the same fields; project-level `cli_path`; extension manifest deploy install/verification/owner-hint/path-root contracts. | Component/portable value is the base. Fleet component override applies when the project belongs to a fleet with a matching component override. Project component override applies after fleet and wins. `Project::cli_path` is a fallback only when neither component value nor explicit fleet/project component override set `cli_path`; extension CLI default/tool comes later in extension deploy code. Extension manifest deploy rules are not a direct override of component deploy fields except via remote-path inference/path roots/install behavior. | Overrideable fields now live in the shared `ComponentOverrideConfig` type in `crates/contracts/homeboy-component-contract/src/config.rs`; `ProjectComponentOverrides` remains a compatibility alias for the existing project/fleet JSON shape. `ComponentOverrideConfig::apply_to_component()` owns the sparse-layer semantics: optional fields replace when present, collection fields replace only when non-empty. The cascade and `cli_path` fallback are implemented in `crates/homeboy-core/src/project/component/overrides.rs`. Component deploy fields are exposed through `deploy_config()` in `crates/contracts/homeboy-component-contract/src/model.rs`. Extension deploy manifest fields are declared in `crates/homeboy-core/src/extension/manifest.rs`. |

### Extension Attachment And Settings

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `extensions`; project `extensions`; rig component `extensions`; extension manifest `settings`; global `settings`; CLI `--setting` / `--setting-json`; flat keys inside `ScopedExtensionConfig`; nested `settings` inside `ScopedExtensionConfig`. | For project-attached components, component portable/registry `extensions` wins when present; project-level `extensions` fills in only when the component has no extensions or an empty map. Within a `ScopedExtensionConfig`, nested `settings` are loaded first and flat keys extend over them, so flat keys win on duplicate setting names. Extension execution context starts with component-scoped extension settings; CLI settings are added later by the runner builder. Global `HomeboyConfig.settings` is a generic settings bag and is not automatically merged into component extension settings by this path. Rig component `extensions` are separate rig-owned bench dispatch config, not automatically merged into component registry config. | Project extension fallback is in `crates/homeboy-core/src/project/component/resolution.rs`. `ScopedExtensionConfig` nested/flat merge is in `crates/contracts/homeboy-component-contract/src/config.rs`. Component extension settings are extracted in `crates/homeboy-core/src/extension/capability.rs` and stored in execution context in `crates/homeboy-core/src/extension/capability.rs`. CLI settings are added by `build_scenario_runner()` in `crates/homeboy-core/src/extension/capability.rs`. Global settings are declared in `crates/homeboy-core/src/defaults.rs`. Rig component extensions are declared in `crates/homeboy-rig/src/spec.rs`. |

### Capability Scripts For Build, Lint, Test, Bench, Fuzz, Trace, Deps

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `scripts.*`; extension manifest capability `extension_script`; extension manifest build `script_names`; extension manifest top-level `scripts` for audit/refactor/fingerprint tooling; legacy component `build_command`. | For build, component `scripts.build` wins, followed by extension bundled build script, followed by local script names declared by the extension. For bench and trace workflows, component scripts win when present; bench uses component scripts only when no extra workloads are supplied, then falls through to extension execution. Trace always uses component trace scripts before extension context. Legacy component `build_command` is rejected as unsupported. Extension top-level `scripts` serve different audit/refactor/fingerprint surfaces and do not participate in command capability selection. | Component script lookup is in `crates/contracts/homeboy-component-contract/src/model.rs`; `has_script()` is in `crates/contracts/homeboy-component-contract/src/model.rs`. Build precedence is documented and implemented in `crates/homeboy-core/src/extension/build/mod.rs`. Bench component-script precedence is in `crates/homeboy-core/src/extension/bench/run/workflow.rs`. Trace component-script precedence is in `crates/homeboy-core/src/extension/trace/run/workflow.rs`. Extension capability resolution is in `crates/homeboy-core/src/extension/capability.rs`. Unsupported `build_command` validation is in `crates/contracts/homeboy-component-contract/src/model.rs`. |

### Runtime Environment Variables

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `env`; server `env`; server runner `env`; runner `env`; extension manifest `env_provider`; rig service `env`; rig executable/tool requirement `env`; extension invocation `env`; CLI/run-specific env additions. | Runner/server runner env is resolved through runner specs; runner `env` is cloned and then normalized to include `homeboy_path`-derived command env. Component env is applied to Homeboy-managed component capability runs and per-run env overrides win by field documentation. Extension env providers are opt-in additions selected by workload/options and passed into scenario runner construction. Rig service env is local to rig service processes, not a component/runtime merge layer. | Component env field and precedence note are in `crates/contracts/homeboy-component-contract/src/model.rs`. Server env and server-runner env are declared in `crates/homeboy-core/src/server/mod.rs` and in `crates/homeboy-core/src/server/mod.rs`. Runner env and spec conversion are in `crates/homeboy-lab-runner/src/lib.rs` and in `crates/homeboy-lab-runner/src/lib.rs`. Extension env provider is declared in `crates/homeboy-core/src/extension/manifest_config.rs` and wired into scenario runners in `crates/homeboy-core/src/extension/capability.rs`. Rig service/env requirement fields are in `crates/homeboy-rig/src/spec.rs` and in `crates/homeboy-rig/src/spec.rs`. |

### Bench Workloads And Bench Defaults

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `scripts.bench`; extension manifest `bench.extension_script`; rig `bench`, `bench_workloads`, `bench_profiles`, `bench.metric_gates`, `bench.accepted_settings`; CLI bench args. | Component `scripts.bench` wins only when no rig/extra workloads are supplied; otherwise extension bench execution is required. Rig `bench` chooses default component(s) and default baseline behavior for rig bench. Rig-owned `bench_workloads` are additive out-of-tree workloads alongside component in-tree discovery. CLI flags such as explicit scenarios, warmup, baseline/ratchet opt-outs, and selected rig profiles override or filter rig defaults at command dispatch; the exact precedence is partly spread outside the schema and workflow file. | Component-script vs extension bench split is in `crates/homeboy-core/src/extension/bench/run/workflow.rs`. Rig bench schema is in `crates/homeboy-rig/src/spec.rs`, bench workloads in `crates/homeboy-rig/src/spec.rs`, and bench profiles in `crates/homeboy-rig/src/spec.rs`. Extension bench config is declared in `crates/homeboy-core/src/extension/manifest_config.rs`. |

### Trace Workloads, Defaults, And Phase Metadata

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `scripts.trace`; extension manifest `trace.extension_script`; rig `trace_workloads`, `trace_workload_defaults`, `trace_phase_templates`, `trace_variants`, `trace_profiles`, `trace_experiments`, `trace_guardrails`; workload-level trace fields. | Component `scripts.trace` wins over extension trace execution. Within rig workload config, workload defaults fill only omitted scalar fields and prepend missing vector fields; workload values remain authoritative. Phase templates fill missing trace phase defaults and merge missing map entries. Variant/profile/experiment resolution is separate trace orchestration and not one global merge with component config. | Trace component-script precedence is in `crates/homeboy-core/src/extension/trace/run/workflow.rs`. Rig trace schemas are declared in `crates/homeboy-rig/src/spec.rs`. Trace config is flattened into workload/default/template in `crates/homeboy-rig/src/spec.rs`, workload fields in `crates/homeboy-rig/src/spec.rs`, and defaults in `crates/homeboy-rig/src/spec.rs`. Default application and template application are in `crates/homeboy-rig/src/spec.rs`; prepend/map semantics are in `crates/homeboy-rig/src/spec.rs`. |

### Test And Audit Selection

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `scripts.test`; extension manifest `test.extension_script`, `test.drift`, `test.changed_file_routing`, `test.passthrough_filter`; extension manifest `audit.test_mapping`; component `audit`; component `scopes`; project/fleet component override `scopes`. | Component `scripts.test` makes the component-owned script the command implementation. Extension manifest test config supplies the extension runner and changed-file/drift behavior when extension execution is selected. Audit test mapping is read only from the extension audit capability. Component `scopes` are base config and can be replaced by fleet/project component overrides through the same override cascade. | Component test scripts use the shared script selection in `crates/contracts/homeboy-component-contract/src/model.rs` and extension capability context in `crates/homeboy-core/src/extension/capability.rs`. Extension test config is declared in `crates/homeboy-core/src/extension/manifest_config.rs`. Audit test mapping accessor is in `crates/homeboy-core/src/extension/manifest.rs`. Overrideable `scopes` are declared in `crates/homeboy-core/src/project/types/component.rs` and applied in `crates/homeboy-core/src/project/component/overrides.rs`. |

### Git/Remote Metadata

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `remote_url`; component `triage_remote_url`; rig component `remote_url`, `triage_remote_url`, `branch`, `ref`, `default_ref`, `stack`; project attachment local path; standalone component registry fallback. | Component portable/registry `remote_url` is effective for normal component operations. For project-attached components, standalone registry `remote_url` fills in only when the portable component lacks it. Rig component Git metadata is used by rig triage/status/materialization paths and is not merged into the component registry. Portable discovery auto-detects `remote_url` from git origin only when absent. `triage_remote_url` is reporting-only by field documentation. | Project standalone fallback for `remote_url` is in `crates/homeboy-core/src/project/component/resolution.rs`. Portable auto-detection is in `crates/homeboy-core/src/component/portable.rs`. Component remote fields are declared in `crates/contracts/homeboy-component-contract/src/model.rs`. Rig component Git fields are declared in `crates/homeboy-rig/src/spec.rs`. |

### Runner And Server Runner Settings

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Standalone runner config; server-embedded runner config; built-in `local` runner; global lab preferred runner; runner env/settings/resources/security. | The `local` runner ID is built in and wins for `runner load local`. For other IDs, standalone local runner config is used only when its kind is `Local`; otherwise loading falls through to a server-embedded runner with the same ID. Runner listing includes the built-in local runner, standalone local runners, then server-embedded runners. Global `lab.preferred_runner` influences default lab runner selection from the listed SSH runners but does not merge runner settings. | Runner load order is in `crates/homeboy-lab-runner/src/lib.rs`. Built-in local runner is in `crates/homeboy-lab-runner/src/lib.rs`. Runner listing composition is in `crates/homeboy-lab-runner/src/lib.rs`. Global lab preferred runner selection begins in `crates/homeboy-lab-runner/src/lib.rs`. Shared runner settings/security structs are declared in `crates/homeboy-core/src/server/mod.rs`. |

### Priority Labels

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Global `triage.priority_labels`; component `priority_labels`; fleet `priority_labels`. | The schemas expose three places, but the actual effective merge order was not fully traced in this investigation window. The global field exists as an optional triage default; component and fleet fields are independent optional labels. Treat precedence as ambiguous until the triage collector paths are audited. | Global triage labels are declared in `crates/homeboy-core/src/defaults.rs`. Component labels are declared in `crates/contracts/homeboy-component-contract/src/model.rs`. Fleet labels are declared in `crates/homeboy-core/src/fleet/mod.rs`. |

### Lifecycle Hooks

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `hooks`; project/fleet component override `hooks`; extension manifest `hooks`; release/deploy lifecycle callers. | Component hooks are base config. Fleet/project component overrides replace the whole hooks map when non-empty, with project overriding fleet. Extension hooks are declared separately; manifest comments state extension hooks run before component hooks at each event, but this investigation did not trace every lifecycle caller, so event-specific behavior should be treated as partially ambiguous. | Component hooks are declared in `crates/contracts/homeboy-component-contract/src/model.rs`. Project/fleet override replacement is in `crates/homeboy-core/src/project/component/overrides.rs` and ordered in `crates/homeboy-core/src/project/component/overrides.rs`. Extension hook declaration and ordering comment are in `crates/homeboy-core/src/extension/manifest.rs`. |

### Artifact And Cleanup Metadata

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `artifact_inputs`; project/fleet component override `artifact_inputs`; component `cleanup_artifacts`; extension manifest build `cleanup_paths`; rig workload `artifact_postprocess`; global `artifact_root`. | Component `artifact_inputs` are replaced by non-empty fleet/project overrides through the same cascade. Component `cleanup_artifacts` are part of deploy config. Extension build `cleanup_paths` and component `cleanup_artifacts` are separate concepts with similar names; this investigation found no direct merge between them in the component override seam. Rig workload `artifact_postprocess` composes workload output post-processing and is not a component deploy cleanup layer. Global `artifact_root` controls persisted run artifact storage, not component artifact inputs. | Artifact inputs are declared in `crates/contracts/homeboy-component-contract/src/model.rs` and overridden in `crates/homeboy-core/src/project/component/overrides.rs`. Cleanup artifacts are exposed in deploy config in `crates/contracts/homeboy-component-contract/src/model.rs`. Extension build cleanup paths are declared in `crates/homeboy-core/src/extension/manifest_config.rs`. Rig workload artifact postprocess is declared in `crates/homeboy-rig/src/spec.rs` and default-prepended in `crates/homeboy-rig/src/spec.rs`. Global artifact root is declared in `crates/homeboy-core/src/defaults.rs`. |

## Ambiguous Or Order-Dependent Areas

| Area | Why it is ambiguous or order-dependent |
| --- | --- |
| Fleet override selection | `resolve_fleet_overrides()` returns the first matching fleet from `fleet::list()` in `crates/homeboy-core/src/project/component/overrides.rs`. `config::list()` sorts entities by ID in `crates/homeboy-core/src/config.rs`, so the winner is deterministic but ID-order-dependent when a project belongs to multiple fleets with matching component overrides. |
| Extension capability ownership | `resolve_extension_for_capability()` errors when multiple linked extensions support the same capability in `crates/homeboy-core/src/extension/capability.rs`; there is no precedence tie-breaker. |
| Priority labels | The schemas overlap, but this pass did not trace the triage collector merge path. Do not assume global/project/fleet/component precedence without auditing triage execution code. |
| Lifecycle hooks | Extension manifest comments state extension hooks run before component hooks, and project/fleet overrides replace component hooks, but event-specific lifecycle callers were not fully traced here. |
| Rig vs component settings | Rig component `extensions`, workload settings/defaults, and component registry `extensions` are separate resolution surfaces. They meet in bench/trace dispatch, but there is no single global merge order across every command. |

## Simplification Proposal

**None of the six removals below have shipped.** They remain proposals from the original #7519 investigation and are recorded here as a backlog, not as a description of current behavior.

These removals preserve currently expressible configuration by moving each concept to the most specific existing home and keeping the already-supported fallback behavior during migration.

1. Remove deploy field duplication from `ProjectComponentAttachment` except `local_path` and `id`.

Rationale: `ProjectComponentAttachment.remote_path` is a third deploy override path in addition to component config and `project.component_overrides.<id>.remote_path`. Current code already lets `project.component_overrides` win over attachment `remote_path`, proven by in `crates/homeboy-core/src/project/component/resolution.rs` plus in `crates/homeboy-core/src/project/component/overrides.rs`. Moving attachment `remote_path` into `component_overrides` keeps every currently expressible project-specific deploy target while making attachments purely about membership and checkout location.

Risk: Existing project configs using attachment `remote_path` need migration. The migration is mechanical: create `component_overrides.<id>.remote_path` with the same value and remove the attachment field.

2. Remove fleet-level `component_overrides` or limit it to fleet-only reporting policy.

Rationale: Fleet component overrides duplicate project component overrides and component config, and the effective fleet winner is ID-order-dependent when multiple fleets match. Keeping deploy overrides at component/project scope retains expressiveness without cross-fleet hidden defaults. If fleet-wide defaults are still needed, model them as an explicit named policy applied by projects instead of implicit membership lookup.

Risk: Multi-project fleets currently use this to avoid repeating project overrides. Removing it increases config repetition unless a replacement policy object or migration fan-out is supplied.

3. Canonicalize extension settings to the flat `extensions.<id>.<key>` shape and remove nested `extensions.<id>.settings` from new config.

Rationale: `ScopedExtensionConfig` already gives flat keys precedence over nested `settings` (in `crates/contracts/homeboy-component-contract/src/config.rs`). Keeping both syntaxes creates duplicate homes inside the same schema without adding expressiveness.

Risk: Existing nested configs need a safe rewrite. Because flat keys already win, migration should fail or warn when nested and flat define different values for the same key.

4. Remove project-level `extensions` fallback after portable config adoption is complete.

Rationale: Project-level extension fallback exists to handle clean tag clones from older releases where `homeboy.json` lacked `extensions` (in `crates/homeboy-core/src/project/component/resolution.rs`). Once repo-local portable config is required for attached components, extension ownership belongs with the component or the rig/workload that invokes it, not a project-wide fallback.

Risk: Older releases without portable extension metadata would need migration or compatibility warnings before removal.

5. Keep `Project::cli_path`, remove per-component `cli_path` overrides where possible.

Rationale: The project struct documents that a CLI entrypoint is usually fixed per project (in `crates/homeboy-core/src/project/mod.rs`). Per-component `cli_path` remains a useful escape hatch today, but most deployments can express the same configuration once at project scope. A staged simplification could warn on redundant per-component values that equal `Project::cli_path`.

Risk: Some components genuinely need a different CLI wrapper. Full removal would lose expressiveness; prefer dedupe warnings first, then keep only explicit exceptions if still needed.

6. Separate component cleanup artifacts from extension build cleanup paths by naming and ownership.

Rationale: Component `cleanup_artifacts` and extension build `cleanup_paths` sound interchangeable but are separate surfaces. Rename or move extension build cleanup to a more explicit extension-owned drift/build-output policy before attempting behavior changes.

Risk: This is documentation/schema cleanup, not a precedence simplification, and should be handled with schema migration plus docs updates.

## Consolidation Shipped In This Slice

The safe code consolidation for this investigation keeps the on-disk contract unchanged and removes the project-local copy of the component override schema:

| Consolidated code | Backward compatibility proof |
| --- | --- |
| `ComponentOverrideConfig` is the canonical field group for component override layers. It lives with component schema types and owns `apply_to_component()`, the one implementation of sparse override semantics for these fields. | `ProjectComponentOverrides` is a type alias, so existing `project.component_overrides` and `fleet.component_overrides` JSON keep the same keys and serde behavior. The config test `project_component_overrides_parse_existing_json_shape` parses the previous JSON shape and verifies every field resolves to the same effective component values. |
| `crates/homeboy-core/src/project/component/overrides.rs` now composes ordered override layers by calling `apply_to_component()` for fleet then project, preserving the documented precedence. | Existing precedence tests still cover project override wins, project `cli_path` fallback, component `cli_path` wins over project fallback, and unset values preserving base component fields. |

No migration or deprecation warning is included in this slice because the serialized config contract is intentionally identical. The remaining removals above require a migration/warning phase before deleting currently accepted config locations.
