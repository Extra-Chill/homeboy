# Configuration Precedence Map

Investigation for Extra-Chill/homeboy#7519. This document maps configuration concepts that appear in two or more schemas and records the current runtime precedence from the resolving code. Precedence here means the effective value used by Homeboy after all participating schemas have been loaded.

## Source Schemas

| Schema | Primary source |
| --- | --- |
| Global config (`homeboy.json`) | `HomeboyConfig` in `src/core/defaults.rs:27` and fields through `src/core/defaults.rs:90` |
| Project config | `Project` in `src/core/project/mod.rs:53` through `src/core/project/mod.rs:131` |
| Component registry and repo-local portable config | `Component` in `src/core/component/model.rs:59` through `src/core/component/model.rs:151`; portable read/write in `src/core/component/portable.rs:6` through `src/core/component/portable.rs:29` and `src/core/component/portable.rs:128` through `src/core/component/portable.rs:156` |
| Rig spec | `RigSpec` in `src/core/rig/spec.rs:38` through `src/core/rig/spec.rs:172`; rig component fields in `src/core/rig/spec.rs:1074` through `src/core/rig/spec.rs:1147` |
| Extension manifest | `ExtensionManifest` in `src/core/extension/manifest.rs:624` through `src/core/extension/manifest.rs:750` |
| Fleet / server / runner / tunnel registry | `ConfigEntity` registry in `src/core/config.rs:176` through `src/core/config.rs:185`; entity implementations in `src/core/fleet/mod.rs:94`, `src/core/server/mod.rs:207`, `src/core/runner/mod.rs:283`, and `src/core/tunnel/entity.rs:32` |

## Overlapping Concepts

### Component Source Path

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component registry `local_path`; repo-local `homeboy.json` discovered from `--path`, positional directory, or CWD; project component attachment `local_path`; rig component `path`; rig component `path_setting`; runner/workspace paths in runner/server config. | Command target resolution: project-scoped component ID wins first when a project is supplied; explicit `--path` then overrides `local_path`; bare directory comes next; CWD/portable checkout for the requested component is preferred before registry lookup; registry lookup follows; CWD discovery is final fallback. Project-attached components replace the resolved `local_path` with the attachment path after applying overrides. Rig component path resolution is a separate path, not merged with component registry unless the rig omits `path` and uses `component_id` / `path_setting`. | `TargetSpec` documents the shared contract at `src/core/component/resolution.rs:8` through `src/core/component/resolution.rs:41`. `resolve_target()` documents and implements the command-facing order at `src/core/component/resolution.rs:415` through `src/core/component/resolution.rs:424` and calls `resolve_effective_inner()` at `src/core/component/resolution.rs:441` through `src/core/component/resolution.rs:447`. Project attachment normalization overwrites the component path at `src/core/project/component/resolution.rs:63` through `src/core/project/component/resolution.rs:79`. Rig component path fields are declared at `src/core/rig/spec.rs:1079` through `src/core/rig/spec.rs:1100`. |

### Component Identity And Aliases

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component registry file stem / `id`; repo-local portable `id`; project component attachment `id`; rig component map key plus optional `component_id`; config entity aliases across project/server/tunnel/component-like registries. | For entity files, the file path ID is authoritative after deserialize because `config::load()` sets the ID from the lookup key. For portable discovery, the repo-local `homeboy.json` must declare a non-empty `id`, which is slugified. For rig components, `component_id` is the registry fallback; when omitted, the map key is the implied component ID. Alias resolution is case-insensitive and happens only when direct entity path lookup misses. | Entity load sets ID and calls `post_load()` at `src/core/config.rs:201` through `src/core/config.rs:221`. Alias fallback is implemented at `src/core/config.rs:224` through `src/core/config.rs:234`. Portable ID validation and slugification are at `src/core/component/portable.rs:38` through `src/core/component/portable.rs:75`. Rig `component_id` semantics are declared at `src/core/rig/spec.rs:1074` through `src/core/rig/spec.rs:1095`. |

### Deploy Target Path

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `remote_path` in registry/portable config; project component attachment `remote_path`; project `component_overrides.<id>.remote_path`; fleet `component_overrides.<id>.remote_path`; extension manifest `deploy.remote_path_inference`; project `path_roots`; extension manifest `deploy.path_roots`; project `base_path`; global deploy defaults for SSH port/artifact prefix/scp flags. | For project-attached components: portable component value loads first; project attachment `remote_path` overrides if non-empty; standalone registry `remote_path` is only a fallback when portable `remote_path` is empty; fleet override is applied next; project override is applied last and wins explicit deploy fields. If the resulting `remote_path` is still empty, extension remote-path inference may fill it. During deploy path expansion, absolute paths are joined safely, managed path roots use project `path_roots`, missing roots can be detected from extension `path_roots.detect_command`, and otherwise the project/base fallback is used or rejected for unsafe `..`. | Attachment override and standalone fallback are at `src/core/project/component/resolution.rs:21` through `src/core/project/component/resolution.rs:63`; standalone fallback fields are limited at `src/core/project/component/resolution.rs:99` through `src/core/project/component/resolution.rs:122`. Fleet then project override order is implemented at `src/core/project/component/overrides.rs:43` through `src/core/project/component/overrides.rs:88`. Extension inference is called after layers at `src/core/project/component/resolution.rs:91` through `src/core/project/component/resolution.rs:94` and implemented at `src/core/component/model.rs:376` through `src/core/component/model.rs:420`. Deploy path-root resolution is implemented at `src/core/deploy/path_roots.rs:20` through `src/core/deploy/path_roots.rs:63`, detection at `src/core/deploy/path_roots.rs:173` through `src/core/deploy/path_roots.rs:213`, project-root matching at `src/core/deploy/path_roots.rs:215` through `src/core/deploy/path_roots.rs:263`, and extension path-root collection at `src/core/deploy/path_roots.rs:298` through `src/core/deploy/path_roots.rs:309`. Global deploy defaults live at `src/core/defaults.rs:323` through `src/core/defaults.rs:334`. |

### Deploy Field Overrides

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `build_artifact`, `extract_command`, `remote_owner`, `deploy_strategy`, `git_deploy`, `artifact_inputs`, `cli_path`, `hooks`; project component overrides for the same fields; fleet component overrides for the same fields; project-level `cli_path`; extension manifest deploy install/verification/owner-hint/path-root contracts. | Component/portable value is the base. Fleet component override applies when the project belongs to a fleet with a matching component override. Project component override applies after fleet and wins. `Project::cli_path` is a fallback only when neither component value nor explicit fleet/project component override set `cli_path`; extension CLI default/tool comes later in extension deploy code. Extension manifest deploy rules are not a direct override of component deploy fields except via remote-path inference/path roots/install behavior. | Overrideable fields now live in the shared `ComponentOverrideConfig` type in `src/core/component/config.rs`; `ProjectComponentOverrides` remains a compatibility alias for the existing project/fleet JSON shape. `ComponentOverrideConfig::apply_to_component()` owns the sparse-layer semantics: optional fields replace when present, collection fields replace only when non-empty. The cascade and `cli_path` fallback are implemented in `src/core/project/component/overrides.rs`. Component deploy fields are exposed through `deploy_config()` at `src/core/component/model.rs:480` through `src/core/component/model.rs:493`. Extension deploy manifest fields are declared at `src/core/extension/manifest.rs:72` through `src/core/extension/manifest.rs:94`. |

### Extension Attachment And Settings

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `extensions`; project `extensions`; rig component `extensions`; extension manifest `settings`; global `settings`; CLI `--setting` / `--setting-json`; flat keys inside `ScopedExtensionConfig`; nested `settings` inside `ScopedExtensionConfig`. | For project-attached components, component portable/registry `extensions` wins when present; project-level `extensions` fills in only when the component has no extensions or an empty map. Within a `ScopedExtensionConfig`, nested `settings` are loaded first and flat keys extend over them, so flat keys win on duplicate setting names. Extension execution context starts with component-scoped extension settings; CLI settings are added later by the runner builder. Global `HomeboyConfig.settings` is a generic settings bag and is not automatically merged into component extension settings by this path. Rig component `extensions` are separate rig-owned bench dispatch config, not automatically merged into component registry config. | Project extension fallback is at `src/core/project/component/resolution.rs:80` through `src/core/project/component/resolution.rs:89`. `ScopedExtensionConfig` nested/flat merge is at `src/core/component/config.rs:20` through `src/core/component/config.rs:78`. Component extension settings are extracted at `src/core/extension/capability.rs:276` through `src/core/extension/capability.rs:292` and stored in execution context at `src/core/extension/capability.rs:364` through `src/core/extension/capability.rs:372`. CLI settings are added by `build_scenario_runner()` at `src/core/extension/capability.rs:132` through `src/core/extension/capability.rs:140`. Global settings are declared at `src/core/defaults.rs:54` through `src/core/defaults.rs:57`. Rig component extensions are declared at `src/core/rig/spec.rs:1134` through `src/core/rig/spec.rs:1141`. |

### Capability Scripts For Build, Lint, Test, Bench, Fuzz, Trace, Deps

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `scripts.*`; extension manifest capability `extension_script`; extension manifest build `script_names`; extension manifest top-level `scripts` for audit/refactor/fingerprint tooling; legacy component `build_command`. | For build, component `scripts.build` wins, followed by extension bundled build script, followed by local script names declared by the extension. For bench and trace workflows, component scripts win when present; bench uses component scripts only when no extra workloads are supplied, then falls through to extension execution. Trace always uses component trace scripts before extension context. Legacy component `build_command` is rejected as unsupported. Extension top-level `scripts` serve different audit/refactor/fingerprint surfaces and do not participate in command capability selection. | Component script lookup is at `src/core/component/model.rs:508` through `src/core/component/model.rs:528`; `has_script()` is at `src/core/component/model.rs:530` through `src/core/component/model.rs:532`. Build precedence is documented and implemented at `src/core/extension/build/mod.rs:52` through `src/core/extension/build/mod.rs:119`. Bench component-script precedence is at `src/core/extension/bench/run/workflow.rs:106` through `src/core/extension/bench/run/workflow.rs:239`. Trace component-script precedence is at `src/core/extension/trace/run/workflow.rs:36` through `src/core/extension/trace/run/workflow.rs:58`. Extension capability resolution is at `src/core/extension/capability.rs:294` through `src/core/extension/capability.rs:373`. Unsupported `build_command` validation is at `src/core/component/model.rs:534` through `src/core/component/model.rs:556`. |

### Runtime Environment Variables

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `env`; server `env`; server runner `env`; runner `env`; extension manifest `env_provider`; rig service `env`; rig executable/tool requirement `env`; extension invocation `env`; CLI/run-specific env additions. | Runner/server runner env is resolved through runner specs; runner `env` is cloned and then normalized to include `homeboy_path`-derived command env. Component env is applied to Homeboy-managed component capability runs and per-run env overrides win by field documentation. Extension env providers are opt-in additions selected by workload/options and passed into scenario runner construction. Rig service env is local to rig service processes, not a component/runtime merge layer. | Component env field and precedence note are at `src/core/component/model.rs:111` through `src/core/component/model.rs:114`. Server env and server-runner env are declared at `src/core/server/mod.rs:57` through `src/core/server/mod.rs:63` and `src/core/server/mod.rs:130` through `src/core/server/mod.rs:142`. Runner env and spec conversion are at `src/core/runner/mod.rs:162` through `src/core/runner/mod.rs:181` and `src/core/runner/mod.rs:192` through `src/core/runner/mod.rs:238`. Extension env provider is declared at `src/core/extension/manifest_config.rs:26` through `src/core/extension/manifest_config.rs:33` and wired into scenario runners at `src/core/extension/capability.rs:127` through `src/core/extension/capability.rs:140`. Rig service/env requirement fields are at `src/core/rig/spec.rs:240` through `src/core/rig/spec.rs:281` and `src/core/rig/spec.rs:1165` through `src/core/rig/spec.rs:1187`. |

### Bench Workloads And Bench Defaults

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `scripts.bench`; extension manifest `bench.extension_script`; rig `bench`, `bench_workloads`, `bench_profiles`, `bench.metric_gates`, `bench.accepted_settings`; CLI bench args. | Component `scripts.bench` wins only when no rig/extra workloads are supplied; otherwise extension bench execution is required. Rig `bench` chooses default component(s) and default baseline behavior for rig bench. Rig-owned `bench_workloads` are additive out-of-tree workloads alongside component in-tree discovery. CLI flags such as explicit scenarios, warmup, baseline/ratchet opt-outs, and selected rig profiles override or filter rig defaults at command dispatch; the exact precedence is partly spread outside the schema and workflow file. | Component-script vs extension bench split is at `src/core/extension/bench/run/workflow.rs:106` through `src/core/extension/bench/run/workflow.rs:239`. Rig bench schema is at `src/core/rig/spec.rs:407` through `src/core/rig/spec.rs:481`, bench workloads at `src/core/rig/spec.rs:100` through `src/core/rig/spec.rs:108`, and bench profiles at `src/core/rig/spec.rs:159` through `src/core/rig/spec.rs:164`. Extension bench config is declared at `src/core/extension/manifest_config.rs:301` through `src/core/extension/manifest_config.rs:305`. |

### Trace Workloads, Defaults, And Phase Metadata

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `scripts.trace`; extension manifest `trace.extension_script`; rig `trace_workloads`, `trace_workload_defaults`, `trace_phase_templates`, `trace_variants`, `trace_profiles`, `trace_experiments`, `trace_guardrails`; workload-level trace fields. | Component `scripts.trace` wins over extension trace execution. Within rig workload config, workload defaults fill only omitted scalar fields and prepend missing vector fields; workload values remain authoritative. Phase templates fill missing trace phase defaults and merge missing map entries. Variant/profile/experiment resolution is separate trace orchestration and not one global merge with component config. | Trace component-script precedence is at `src/core/extension/trace/run/workflow.rs:36` through `src/core/extension/trace/run/workflow.rs:58`. Rig trace schemas are declared at `src/core/rig/spec.rs:110` through `src/core/rig/spec.rs:152`. Trace config is flattened into workload/default/template at `src/core/rig/spec.rs:534` through `src/core/rig/spec.rs:550`, workload fields at `src/core/rig/spec.rs:552` through `src/core/rig/spec.rs:597`, and defaults at `src/core/rig/spec.rs:599` through `src/core/rig/spec.rs:636`. Default application and template application are at `src/core/rig/spec.rs:644` through `src/core/rig/spec.rs:697`; prepend/map semantics are at `src/core/rig/spec.rs:699` through `src/core/rig/spec.rs:716`. |

### Test And Audit Selection

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `scripts.test`; extension manifest `test.extension_script`, `test.drift`, `test.changed_file_routing`, `test.passthrough_filter`; extension manifest `audit.test_mapping`; component `audit`; component `scopes`; project/fleet component override `scopes`. | Component `scripts.test` makes the component-owned script the command implementation. Extension manifest test config supplies the extension runner and changed-file/drift behavior when extension execution is selected. Audit test mapping is read only from the extension audit capability. Component `scopes` are base config and can be replaced by fleet/project component overrides through the same override cascade. | Component test scripts use the shared script selection at `src/core/component/model.rs:508` through `src/core/component/model.rs:528` and extension capability context at `src/core/extension/capability.rs:294` through `src/core/extension/capability.rs:373`. Extension test config is declared at `src/core/extension/manifest_config.rs:250` through `src/core/extension/manifest_config.rs:269`. Audit test mapping accessor is at `src/core/extension/manifest.rs:896` through `src/core/extension/manifest.rs:907`. Overrideable `scopes` are declared at `src/core/project/types/component.rs:34` through `src/core/project/types/component.rs:35` and applied at `src/core/project/component/overrides.rs:32` through `src/core/project/component/overrides.rs:34`. |

### Git/Remote Metadata

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `remote_url`; component `triage_remote_url`; rig component `remote_url`, `triage_remote_url`, `branch`, `ref`, `default_ref`, `stack`; project attachment local path; standalone component registry fallback. | Component portable/registry `remote_url` is effective for normal component operations. For project-attached components, standalone registry `remote_url` fills in only when the portable component lacks it. Rig component Git metadata is used by rig triage/status/materialization paths and is not merged into the component registry. Portable discovery auto-detects `remote_url` from git origin only when absent. `triage_remote_url` is reporting-only by field documentation. | Project standalone fallback for `remote_url` is at `src/core/project/component/resolution.rs:119` through `src/core/project/component/resolution.rs:121`. Portable auto-detection is at `src/core/component/portable.rs:288` through `src/core/component/portable.rs:293`. Component remote fields are declared at `src/core/component/model.rs:87` through `src/core/component/model.rs:98`. Rig component Git fields are declared at `src/core/rig/spec.rs:1102` through `src/core/rig/spec.rs:1133`. |

### Runner And Server Runner Settings

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Standalone runner config; server-embedded runner config; built-in `local` runner; global lab preferred runner; runner env/settings/resources/security. | The `local` runner ID is built in and wins for `runner load local`. For other IDs, standalone local runner config is used only when its kind is `Local`; otherwise loading falls through to a server-embedded runner with the same ID. Runner listing includes the built-in local runner, standalone local runners, then server-embedded runners. Global `lab.preferred_runner` influences default lab runner selection from the listed SSH runners but does not merge runner settings. | Runner load order is at `src/core/runner/mod.rs:322` through `src/core/runner/mod.rs:334`. Built-in local runner is at `src/core/runner/mod.rs:336` through `src/core/runner/mod.rs:350`. Runner listing composition is at `src/core/runner/mod.rs:357` through `src/core/runner/mod.rs:372`. Global lab preferred runner selection begins at `src/core/runner/mod.rs:375` through `src/core/runner/mod.rs:400`. Shared runner settings/security structs are declared at `src/core/server/mod.rs:65` through `src/core/server/mod.rs:142`. |

### Priority Labels

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Global `triage.priority_labels`; component `priority_labels`; fleet `priority_labels`. | The schemas expose three places, but the actual effective merge order was not fully traced in this investigation window. The global field exists as an optional triage default; component and fleet fields are independent optional labels. Treat precedence as ambiguous until the triage collector paths are audited. | Global triage labels are declared at `src/core/defaults.rs:185` through `src/core/defaults.rs:189`. Component labels are declared at `src/core/component/model.rs:96` through `src/core/component/model.rs:101`. Fleet labels are declared at `src/core/fleet/mod.rs:39` through `src/core/fleet/mod.rs:41`. |

### Lifecycle Hooks

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `hooks`; project/fleet component override `hooks`; extension manifest `hooks`; release/deploy lifecycle callers. | Component hooks are base config. Fleet/project component overrides replace the whole hooks map when non-empty, with project overriding fleet. Extension hooks are declared separately; manifest comments state extension hooks run before component hooks at each event, but this investigation did not trace every lifecycle caller, so event-specific behavior should be treated as partially ambiguous. | Component hooks are declared at `src/core/component/model.rs:80` through `src/core/component/model.rs:82`. Project/fleet override replacement is at `src/core/project/component/overrides.rs:29` through `src/core/project/component/overrides.rs:31` and ordered at `src/core/project/component/overrides.rs:68` through `src/core/project/component/overrides.rs:77`. Extension hook declaration and ordering comment are at `src/core/extension/manifest.rs:732` through `src/core/extension/manifest.rs:735`. |

### Artifact And Cleanup Metadata

| Locations | Effective precedence | Resolving code path |
| --- | --- | --- |
| Component `artifact_inputs`; project/fleet component override `artifact_inputs`; component `cleanup_artifacts`; extension manifest build `cleanup_paths`; rig workload `artifact_postprocess`; global `artifact_root`. | Component `artifact_inputs` are replaced by non-empty fleet/project overrides through the same cascade. Component `cleanup_artifacts` are part of deploy config. Extension build `cleanup_paths` and component `cleanup_artifacts` are separate concepts with similar names; this investigation found no direct merge between them in the component override seam. Rig workload `artifact_postprocess` composes workload output post-processing and is not a component deploy cleanup layer. Global `artifact_root` controls persisted run artifact storage, not component artifact inputs. | Artifact inputs are declared at `src/core/component/model.rs:125` through `src/core/component/model.rs:126` and overridden at `src/core/project/component/overrides.rs:35` through `src/core/project/component/overrides.rs:37`. Cleanup artifacts are exposed in deploy config at `src/core/component/model.rs:480` through `src/core/component/model.rs:493`. Extension build cleanup paths are declared at `src/core/extension/manifest_config.rs:195` through `src/core/extension/manifest_config.rs:215`. Rig workload artifact postprocess is declared at `src/core/rig/spec.rs:552` through `src/core/rig/spec.rs:561` and default-prepended at `src/core/rig/spec.rs:663` through `src/core/rig/spec.rs:667`. Global artifact root is declared at `src/core/defaults.rs:67` through `src/core/defaults.rs:72`. |

## Ambiguous Or Order-Dependent Areas

| Area | Why it is ambiguous or order-dependent |
| --- | --- |
| Fleet override selection | `resolve_fleet_overrides()` returns the first matching fleet from `fleet::list()` at `src/core/project/component/overrides.rs:91` through `src/core/project/component/overrides.rs:110`. `config::list()` sorts entities by ID at `src/core/config.rs:260` through `src/core/config.rs:299`, so the winner is deterministic but ID-order-dependent when a project belongs to multiple fleets with matching component overrides. |
| Extension capability ownership | `resolve_extension_for_capability()` errors when multiple linked extensions support the same capability at `src/core/extension/capability.rs:294` through `src/core/extension/capability.rs:317`; there is no precedence tie-breaker. |
| Priority labels | The schemas overlap, but this pass did not trace the triage collector merge path. Do not assume global/project/fleet/component precedence without auditing triage execution code. |
| Lifecycle hooks | Extension manifest comments state extension hooks run before component hooks, and project/fleet overrides replace component hooks, but event-specific lifecycle callers were not fully traced here. |
| Rig vs component settings | Rig component `extensions`, workload settings/defaults, and component registry `extensions` are separate resolution surfaces. They meet in bench/trace dispatch, but there is no single global merge order across every command. |

## Simplification Proposal

These removals preserve currently expressible configuration by moving each concept to the most specific existing home and keeping the already-supported fallback behavior during migration.

1. Remove deploy field duplication from `ProjectComponentAttachment` except `local_path` and `id`.

Rationale: `ProjectComponentAttachment.remote_path` is a third deploy override path in addition to component config and `project.component_overrides.<id>.remote_path`. Current code already lets `project.component_overrides` win over attachment `remote_path`, proven by `src/core/project/component/resolution.rs:55` through `src/core/project/component/resolution.rs:63` plus `src/core/project/component/overrides.rs:73` through `src/core/project/component/overrides.rs:77`. Moving attachment `remote_path` into `component_overrides` keeps every currently expressible project-specific deploy target while making attachments purely about membership and checkout location.

Risk: Existing project configs using attachment `remote_path` need migration. The migration is mechanical: create `component_overrides.<id>.remote_path` with the same value and remove the attachment field.

2. Remove fleet-level `component_overrides` or limit it to fleet-only reporting policy.

Rationale: Fleet component overrides duplicate project component overrides and component config, and the effective fleet winner is ID-order-dependent when multiple fleets match. Keeping deploy overrides at component/project scope retains expressiveness without cross-fleet hidden defaults. If fleet-wide defaults are still needed, model them as an explicit named policy applied by projects instead of implicit membership lookup.

Risk: Multi-project fleets currently use this to avoid repeating project overrides. Removing it increases config repetition unless a replacement policy object or migration fan-out is supplied.

3. Canonicalize extension settings to the flat `extensions.<id>.<key>` shape and remove nested `extensions.<id>.settings` from new config.

Rationale: `ScopedExtensionConfig` already gives flat keys precedence over nested `settings` (`src/core/component/config.rs:66` through `src/core/component/config.rs:75`). Keeping both syntaxes creates duplicate homes inside the same schema without adding expressiveness.

Risk: Existing nested configs need a safe rewrite. Because flat keys already win, migration should fail or warn when nested and flat define different values for the same key.

4. Remove project-level `extensions` fallback after portable config adoption is complete.

Rationale: Project-level extension fallback exists to handle clean tag clones from older releases where `homeboy.json` lacked `extensions` (`src/core/project/component/resolution.rs:80` through `src/core/project/component/resolution.rs:89`). Once repo-local portable config is required for attached components, extension ownership belongs with the component or the rig/workload that invokes it, not a project-wide fallback.

Risk: Older releases without portable extension metadata would need migration or compatibility warnings before removal.

5. Keep `Project::cli_path`, remove per-component `cli_path` overrides where possible.

Rationale: The project struct documents that a CLI entrypoint is usually fixed per project (`src/core/project/mod.rs:92` through `src/core/project/mod.rs:104`). Per-component `cli_path` remains a useful escape hatch today, but most deployments can express the same configuration once at project scope. A staged simplification could warn on redundant per-component values that equal `Project::cli_path`.

Risk: Some components genuinely need a different CLI wrapper. Full removal would lose expressiveness; prefer dedupe warnings first, then keep only explicit exceptions if still needed.

6. Separate component cleanup artifacts from extension build cleanup paths by naming and ownership.

Rationale: Component `cleanup_artifacts` and extension build `cleanup_paths` sound interchangeable but are separate surfaces. Rename or move extension build cleanup to a more explicit extension-owned drift/build-output policy before attempting behavior changes.

Risk: This is documentation/schema cleanup, not a precedence simplification, and should be handled with schema migration plus docs updates.

## Consolidation Shipped In This Slice

The safe code consolidation for this investigation keeps the on-disk contract unchanged and removes the project-local copy of the component override schema:

| Consolidated code | Backward compatibility proof |
| --- | --- |
| `ComponentOverrideConfig` is the canonical field group for component override layers. It lives with component schema types and owns `apply_to_component()`, the one implementation of sparse override semantics for these fields. | `ProjectComponentOverrides` is a type alias, so existing `project.component_overrides` and `fleet.component_overrides` JSON keep the same keys and serde behavior. The config test `project_component_overrides_parse_existing_json_shape` parses the previous JSON shape and verifies every field resolves to the same effective component values. |
| `src/core/project/component/overrides.rs` now composes ordered override layers by calling `apply_to_component()` for fleet then project, preserving the documented precedence. | Existing precedence tests still cover project override wins, project `cli_path` fallback, component `cli_path` wins over project fallback, and unset values preserving base component fields. |

No migration or deprecation warning is included in this slice because the serialized config contract is intentionally identical. The remaining removals above require a migration/warning phase before deleting currently accepted config locations.
