<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy fuzz` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/fuzz.md](../../../commands/fuzz.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy fuzz`

```sh
homeboy fuzz [OPTIONS] [COMPONENT] [ARGS]... [COMMAND]
```

Run generic fuzz workloads for a component

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |
| `[ARGS]...` | no | Additional runner arguments reserved for the fuzz extension script |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--rig` | `<RIG_ID>` | Run against a rig's component path, extension config, and rig-declared fuzz workloads |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--workload` | `<ID>` | Extension-declared workload id to select |
| `--profile` | `<ID>` | Rig-defined fuzz profile to select. Without --rig, `lab` expands the generic safe Lab evidence-run defaults |
| `--shared-state` | `<DIR>` | Shared state directory handed to the fuzz runner. Homeboy forwards the path as HOMEBOY_FUZZ_SHARED_STATE; the runner owns any mutation policy |
| `--run-id` | `<ID>` | Stable caller-supplied proof label for downstream fuzz runners |
| `--tracker-ref` | `<KIND:ID>` | Product-agnostic tracker anchor for this fuzz run. Repeatable. Format: KIND:ID |
| `--seed` | `<SEED>` | Deterministic seed forwarded by future fuzz runners |
| `--inventory` | `<PATH>` | Product-neutral fuzz target inventory JSON discovered before execution |
| `--sequence-plan` | `<PATH>` | Exact generated generic sequence plan JSON (`homeboy/fuzz-sequence-plan/v1`) to hand to the runner |
| `--require-case-log` | flag | Fail the run unless the campaign links case-level execution evidence |
| `--require-coverage-summary` | flag | Fail the run unless the campaign includes or links a coverage summary |
| `--require-result-envelope` | flag | Fail the run unless the campaign links a result-envelope artifact |
| `--max-duration` | `<DURATION>` | Maximum runtime budget forwarded by future fuzz runners, e.g. 60s or 5m |
| `--gate-profile` | `<GATE_PROFILE>` | Required artifact and gate profile to request from the fuzz runner Values: `measurement`, `evidence`, `coverage-complete`, `strict`. |
| `--allow-destructive` | flag | Permit destructive fuzz operations when verified generic isolation proof is present |
| `--isolation` | `<ISOLATION>` | Requested generic runner isolation contract for the fuzz run. This flag is advisory; destructive fuzz also requires verified isolation proof from the run context Values: `shared`, `isolated`. |
| `--isolation-proof` | `<PATH>` | Explicit homeboy/isolation-proof/v1 JSON proving destructive fuzz can run safely |
| `--allow-local-destructive-fuzz` | flag | Permit destructive fuzz to execute on the local controller instead of Lab |
| `--expect-metric` | `<METRIC=VALUE>` | Require a numeric metric emitted by the fuzz campaign to equal this value. Repeatable. Format: `--expect-metric metric_name=2` |
| `--action-model` | `<PATH>` | Generic action model contract JSON (`homeboy/fuzz-action-model/v1`) to include in the execution request |
| `--exploration-policy` | `<PATH>` | Generic exploration policy contract JSON (`homeboy/fuzz-exploration-policy/v1`) to include in the execution request |

| Subcommand | Summary |
| --- | --- |
| `homeboy fuzz contract` | Print the product-neutral fuzz schema contract |
| `homeboy fuzz doctor` | Diagnose active fuzz runtime provenance and installed extension revision |
| `homeboy fuzz discover` | Normalize and merge discovered fuzz target inventory artifacts |
| `homeboy fuzz list` | List declared fuzz workloads without executing them |
| `homeboy fuzz plan` | Build a fuzz execution request without executing it |
| `homeboy fuzz stable` | Plan stable workload Lab commands from a manifest without executing them |
| `homeboy fuzz run-campaign` | Execute or dry-run a generated fuzz campaign plan |
| `homeboy fuzz run` | Execute the selected fuzz workload, persist fuzz evidence, and surface its campaign contract |
| `homeboy fuzz validate` | Validate a fuzz result campaign file |
| `homeboy fuzz report` | Persist a result envelope from a fuzz campaign file |
| `homeboy fuzz compare` | Compare two persisted fuzz result envelopes |
| `homeboy fuzz replay` | Resolve replay metadata for persisted fuzz cases |
| `homeboy fuzz minimize` | Resolve minimization metadata for persisted fuzz cases |
| `homeboy fuzz inspect` | Print a compact fuzz failure diagnosis or the complete runner result |

## `homeboy fuzz contract`

```sh
homeboy fuzz contract
```

Print the product-neutral fuzz schema contract

## `homeboy fuzz doctor`

```sh
homeboy fuzz doctor [OPTIONS]
```

Diagnose active fuzz runtime provenance and installed extension revision

| Option | Value | Description |
| --- | --- | --- |
| `--extension` | `<ID>` | Extension whose active install should be diagnosed |

## `homeboy fuzz discover`

```sh
homeboy fuzz discover [OPTIONS]
```

Normalize and merge discovered fuzz target inventory artifacts

| Option | Value | Description |
| --- | --- | --- |
| `--inventory` | `<PATH>` | Existing fuzz target inventory artifact to ingest |
| `--inventory-id` | `<ID>` | Stable id for the merged inventory artifact |
| `--source-label` | `<LABEL>` | Human-readable source label recorded in merged provenance |

## `homeboy fuzz list`

```sh
homeboy fuzz list [OPTIONS] [COMPONENT]
```

List declared fuzz workloads without executing them

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--rig` | `<RIG_ID>` | Discover workloads using a rig's component path, extension config, and rig-declared fuzz workloads |
| `--remote-discovery` | flag | Query the selected runner for runner-specific availability. Without this flag list reads only local installed rig metadata |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |

## `homeboy fuzz plan`

```sh
homeboy fuzz plan [OPTIONS] [COMPONENT] [ARGS]...
```

Build a fuzz execution request without executing it

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |
| `[ARGS]...` | no | Additional runner arguments reserved for the fuzz extension script |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--rig` | `<RIG_ID>` | Run against a rig's component path, extension config, and rig-declared fuzz workloads |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--workload` | `<ID>` | Extension-declared workload id to select |
| `--profile` | `<ID>` | Rig-defined fuzz profile to select. Without --rig, `lab` expands the generic safe Lab evidence-run defaults |
| `--shared-state` | `<DIR>` | Shared state directory handed to the fuzz runner. Homeboy forwards the path as HOMEBOY_FUZZ_SHARED_STATE; the runner owns any mutation policy |
| `--run-id` | `<ID>` | Stable caller-supplied proof label for downstream fuzz runners |
| `--tracker-ref` | `<KIND:ID>` | Product-agnostic tracker anchor for this fuzz run. Repeatable. Format: KIND:ID |
| `--seed` | `<SEED>` | Deterministic seed forwarded by future fuzz runners |
| `--inventory` | `<PATH>` | Product-neutral fuzz target inventory JSON discovered before execution |
| `--sequence-plan` | `<PATH>` | Exact generated generic sequence plan JSON (`homeboy/fuzz-sequence-plan/v1`) to hand to the runner |
| `--require-case-log` | flag | Fail the run unless the campaign links case-level execution evidence |
| `--require-coverage-summary` | flag | Fail the run unless the campaign includes or links a coverage summary |
| `--require-result-envelope` | flag | Fail the run unless the campaign links a result-envelope artifact |
| `--max-duration` | `<DURATION>` | Maximum runtime budget forwarded by future fuzz runners, e.g. 60s or 5m |
| `--gate-profile` | `<GATE_PROFILE>` | Required artifact and gate profile to request from the fuzz runner Values: `measurement`, `evidence`, `coverage-complete`, `strict`. |
| `--allow-destructive` | flag | Permit destructive fuzz operations when verified generic isolation proof is present |
| `--isolation` | `<ISOLATION>` | Requested generic runner isolation contract for the fuzz run. This flag is advisory; destructive fuzz also requires verified isolation proof from the run context Values: `shared`, `isolated`. |
| `--isolation-proof` | `<PATH>` | Explicit homeboy/isolation-proof/v1 JSON proving destructive fuzz can run safely |
| `--allow-local-destructive-fuzz` | flag | Permit destructive fuzz to execute on the local controller instead of Lab |
| `--expect-metric` | `<METRIC=VALUE>` | Require a numeric metric emitted by the fuzz campaign to equal this value. Repeatable. Format: `--expect-metric metric_name=2` |
| `--action-model` | `<PATH>` | Generic action model contract JSON (`homeboy/fuzz-action-model/v1`) to include in the execution request |
| `--exploration-policy` | `<PATH>` | Generic exploration policy contract JSON (`homeboy/fuzz-exploration-policy/v1`) to include in the execution request |
| `--request-id` | `<ID>` | Stable request id. Defaults to --run-id, then the selected workload id |
| `--strategy` | `<STRATEGY>` | Inventory selection strategy Values: `all`, `read-only`, `crud`, `coverage-gaps`. |
| `--operation` | `<FILTER>` | Select operations by canonical family, operation kind, or operation id |
| `--operation-family` | `<FAMILY>` | Select operations by canonical family |
| `--case-budget` | `<COUNT>` | Maximum number of cases the downstream runner should generate |
| `--duration-budget-seconds` | `<SECONDS>` | Maximum execution budget in seconds for downstream runners |
| `--campaign-manifest` | `<PATH>` | Product-neutral campaign manifest containing workload ids and optional planning metadata |
| `--campaign-workload` | `<ID>` | Add a workload id to the generated campaign plan. Repeatable |
| `--lab-runner` | `<ID>` | Preferred Lab runner id to record in campaign plan entries without executing them. Prefer the global `--runner` spelling; this alias remains compatible with existing manifests and automation |
| `--required-artifact` | `<ID>` | Additional required artifact id/kind expected from every campaign entry. Repeatable |
| `--execute` | flag | Execute generated campaign entries through the existing `fuzz run` primitive |
| `--dry-run` | flag | Emit structured dispatch records without executing campaign entries |
| `--resume` | flag | Skip campaign entries whose run id already exists in the persisted run store |

## `homeboy fuzz stable`

```sh
homeboy fuzz stable <COMMAND>
```

Plan stable workload Lab commands from a manifest without executing them

| Subcommand | Summary |
| --- | --- |
| `homeboy fuzz stable plan` | Emit stable workload Lab command JSON from a stable workload manifest |

## `homeboy fuzz stable plan`

```sh
homeboy fuzz stable plan [OPTIONS]
```

Emit stable workload Lab command JSON from a stable workload manifest

| Option | Value | Description |
| --- | --- | --- |
| `--manifest` | `<PATH>` | Stable workload manifest JSON path |
| `--stable-id` | `<ID[,ID]>` | Limit to one or more stable workload ids. Repeatable and comma-separated |
| `--runner` | `<ID>` | Preferred Lab runner id to include in every command |
| `--artifact-root` | `<DIR>` | Persisted artifact root to include in every command |
| `--run-id-prefix` | `<ID>` | Stable run id prefix. Defaults to stable-YYYYMMDD |
| `--tracker-ref` | `<KIND:ID>` | Extra tracker ref added to every run command. Repeatable. Format: KIND:ID |
| `--detach-after-handoff` | flag | Return after the Lab daemon accepts each run |
| `--component` | `<ID>` | Optional component id for comparison commands |
| `--since` | `<SINCE>` | Lookback for refs compare command |
| `--limit` | `<LIMIT>` | Run-history limit for refs/compare commands |
| `--hotspot-limit` | `<HOTSPOT_LIMIT>` | Hotspot compare row limit |

## `homeboy fuzz run-campaign`

```sh
homeboy fuzz run-campaign [OPTIONS] [COMPONENT] [ARGS]...
```

Execute or dry-run a generated fuzz campaign plan

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |
| `[ARGS]...` | no | Additional runner arguments reserved for the fuzz extension script |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--rig` | `<RIG_ID>` | Run against a rig's component path, extension config, and rig-declared fuzz workloads |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--workload` | `<ID>` | Extension-declared workload id to select |
| `--profile` | `<ID>` | Rig-defined fuzz profile to select. Without --rig, `lab` expands the generic safe Lab evidence-run defaults |
| `--shared-state` | `<DIR>` | Shared state directory handed to the fuzz runner. Homeboy forwards the path as HOMEBOY_FUZZ_SHARED_STATE; the runner owns any mutation policy |
| `--run-id` | `<ID>` | Stable caller-supplied proof label for downstream fuzz runners |
| `--tracker-ref` | `<KIND:ID>` | Product-agnostic tracker anchor for this fuzz run. Repeatable. Format: KIND:ID |
| `--seed` | `<SEED>` | Deterministic seed forwarded by future fuzz runners |
| `--inventory` | `<PATH>` | Product-neutral fuzz target inventory JSON discovered before execution |
| `--sequence-plan` | `<PATH>` | Exact generated generic sequence plan JSON (`homeboy/fuzz-sequence-plan/v1`) to hand to the runner |
| `--require-case-log` | flag | Fail the run unless the campaign links case-level execution evidence |
| `--require-coverage-summary` | flag | Fail the run unless the campaign includes or links a coverage summary |
| `--require-result-envelope` | flag | Fail the run unless the campaign links a result-envelope artifact |
| `--max-duration` | `<DURATION>` | Maximum runtime budget forwarded by future fuzz runners, e.g. 60s or 5m |
| `--gate-profile` | `<GATE_PROFILE>` | Required artifact and gate profile to request from the fuzz runner Values: `measurement`, `evidence`, `coverage-complete`, `strict`. |
| `--allow-destructive` | flag | Permit destructive fuzz operations when verified generic isolation proof is present |
| `--isolation` | `<ISOLATION>` | Requested generic runner isolation contract for the fuzz run. This flag is advisory; destructive fuzz also requires verified isolation proof from the run context Values: `shared`, `isolated`. |
| `--isolation-proof` | `<PATH>` | Explicit homeboy/isolation-proof/v1 JSON proving destructive fuzz can run safely |
| `--allow-local-destructive-fuzz` | flag | Permit destructive fuzz to execute on the local controller instead of Lab |
| `--expect-metric` | `<METRIC=VALUE>` | Require a numeric metric emitted by the fuzz campaign to equal this value. Repeatable. Format: `--expect-metric metric_name=2` |
| `--action-model` | `<PATH>` | Generic action model contract JSON (`homeboy/fuzz-action-model/v1`) to include in the execution request |
| `--exploration-policy` | `<PATH>` | Generic exploration policy contract JSON (`homeboy/fuzz-exploration-policy/v1`) to include in the execution request |
| `--request-id` | `<ID>` | Stable request id. Defaults to --run-id, then the selected workload id |
| `--strategy` | `<STRATEGY>` | Inventory selection strategy Values: `all`, `read-only`, `crud`, `coverage-gaps`. |
| `--operation` | `<FILTER>` | Select operations by canonical family, operation kind, or operation id |
| `--operation-family` | `<FAMILY>` | Select operations by canonical family |
| `--case-budget` | `<COUNT>` | Maximum number of cases the downstream runner should generate |
| `--duration-budget-seconds` | `<SECONDS>` | Maximum execution budget in seconds for downstream runners |
| `--campaign-manifest` | `<PATH>` | Product-neutral campaign manifest containing workload ids and optional planning metadata |
| `--campaign-workload` | `<ID>` | Add a workload id to the generated campaign plan. Repeatable |
| `--lab-runner` | `<ID>` | Preferred Lab runner id to record in campaign plan entries without executing them. Prefer the global `--runner` spelling; this alias remains compatible with existing manifests and automation |
| `--required-artifact` | `<ID>` | Additional required artifact id/kind expected from every campaign entry. Repeatable |
| `--execute` | flag | Execute generated campaign entries through the existing `fuzz run` primitive |
| `--dry-run` | flag | Emit structured dispatch records without executing campaign entries |
| `--resume` | flag | Skip campaign entries whose run id already exists in the persisted run store |

## `homeboy fuzz run`

```sh
homeboy fuzz run [OPTIONS] [COMPONENT] [ARGS]...
```

Execute the selected fuzz workload, persist fuzz evidence, and surface its campaign contract

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |
| `[ARGS]...` | no | Additional runner arguments reserved for the fuzz extension script |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--rig` | `<RIG_ID>` | Run against a rig's component path, extension config, and rig-declared fuzz workloads |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--workload` | `<ID>` | Extension-declared workload id to select |
| `--profile` | `<ID>` | Rig-defined fuzz profile to select. Without --rig, `lab` expands the generic safe Lab evidence-run defaults |
| `--shared-state` | `<DIR>` | Shared state directory handed to the fuzz runner. Homeboy forwards the path as HOMEBOY_FUZZ_SHARED_STATE; the runner owns any mutation policy |
| `--run-id` | `<ID>` | Stable caller-supplied proof label for downstream fuzz runners |
| `--tracker-ref` | `<KIND:ID>` | Product-agnostic tracker anchor for this fuzz run. Repeatable. Format: KIND:ID |
| `--seed` | `<SEED>` | Deterministic seed forwarded by future fuzz runners |
| `--inventory` | `<PATH>` | Product-neutral fuzz target inventory JSON discovered before execution |
| `--sequence-plan` | `<PATH>` | Exact generated generic sequence plan JSON (`homeboy/fuzz-sequence-plan/v1`) to hand to the runner |
| `--require-case-log` | flag | Fail the run unless the campaign links case-level execution evidence |
| `--require-coverage-summary` | flag | Fail the run unless the campaign includes or links a coverage summary |
| `--require-result-envelope` | flag | Fail the run unless the campaign links a result-envelope artifact |
| `--max-duration` | `<DURATION>` | Maximum runtime budget forwarded by future fuzz runners, e.g. 60s or 5m |
| `--gate-profile` | `<GATE_PROFILE>` | Required artifact and gate profile to request from the fuzz runner Values: `measurement`, `evidence`, `coverage-complete`, `strict`. |
| `--allow-destructive` | flag | Permit destructive fuzz operations when verified generic isolation proof is present |
| `--isolation` | `<ISOLATION>` | Requested generic runner isolation contract for the fuzz run. This flag is advisory; destructive fuzz also requires verified isolation proof from the run context Values: `shared`, `isolated`. |
| `--isolation-proof` | `<PATH>` | Explicit homeboy/isolation-proof/v1 JSON proving destructive fuzz can run safely |
| `--allow-local-destructive-fuzz` | flag | Permit destructive fuzz to execute on the local controller instead of Lab |
| `--expect-metric` | `<METRIC=VALUE>` | Require a numeric metric emitted by the fuzz campaign to equal this value. Repeatable. Format: `--expect-metric metric_name=2` |
| `--action-model` | `<PATH>` | Generic action model contract JSON (`homeboy/fuzz-action-model/v1`) to include in the execution request |
| `--exploration-policy` | `<PATH>` | Generic exploration policy contract JSON (`homeboy/fuzz-exploration-policy/v1`) to include in the execution request |

## `homeboy fuzz validate`

```sh
homeboy fuzz validate [OPTIONS] <RESULTS_FILE>
```

Validate a fuzz result campaign file

| Argument | Required | Description |
| --- | --- | --- |
| `<RESULTS_FILE>` | yes | Fuzz campaign JSON file emitted by a runner |

| Option | Value | Description |
| --- | --- | --- |
| `--gate-profile` | `<GATE_PROFILE>` | Gate profile to evaluate while validating the campaign Values: `measurement`, `evidence`, `coverage-complete`, `strict`. |
| `--case-log` | `<PATH>` | Canonical fuzz case log JSONL/JSON artifact to validate |

## `homeboy fuzz report`

```sh
homeboy fuzz report [OPTIONS] <RESULTS_FILE> [COMPONENT] [ARGS]...
```

Persist a result envelope from a fuzz campaign file

| Argument | Required | Description |
| --- | --- | --- |
| `<RESULTS_FILE>` | yes | Fuzz campaign JSON file emitted by a runner |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |
| `[ARGS]...` | no | Additional runner arguments reserved for the fuzz extension script |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--rig` | `<RIG_ID>` | Run against a rig's component path, extension config, and rig-declared fuzz workloads |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--workload` | `<ID>` | Extension-declared workload id to select |
| `--profile` | `<ID>` | Rig-defined fuzz profile to select. Without --rig, `lab` expands the generic safe Lab evidence-run defaults |
| `--shared-state` | `<DIR>` | Shared state directory handed to the fuzz runner. Homeboy forwards the path as HOMEBOY_FUZZ_SHARED_STATE; the runner owns any mutation policy |
| `--run-id` | `<ID>` | Stable caller-supplied proof label for downstream fuzz runners |
| `--tracker-ref` | `<KIND:ID>` | Product-agnostic tracker anchor for this fuzz run. Repeatable. Format: KIND:ID |
| `--seed` | `<SEED>` | Deterministic seed forwarded by future fuzz runners |
| `--inventory` | `<PATH>` | Product-neutral fuzz target inventory JSON discovered before execution |
| `--sequence-plan` | `<PATH>` | Exact generated generic sequence plan JSON (`homeboy/fuzz-sequence-plan/v1`) to hand to the runner |
| `--require-case-log` | flag | Fail the run unless the campaign links case-level execution evidence |
| `--require-coverage-summary` | flag | Fail the run unless the campaign includes or links a coverage summary |
| `--require-result-envelope` | flag | Fail the run unless the campaign links a result-envelope artifact |
| `--max-duration` | `<DURATION>` | Maximum runtime budget forwarded by future fuzz runners, e.g. 60s or 5m |
| `--gate-profile` | `<GATE_PROFILE>` | Required artifact and gate profile to request from the fuzz runner Values: `measurement`, `evidence`, `coverage-complete`, `strict`. |
| `--allow-destructive` | flag | Permit destructive fuzz operations when verified generic isolation proof is present |
| `--isolation` | `<ISOLATION>` | Requested generic runner isolation contract for the fuzz run. This flag is advisory; destructive fuzz also requires verified isolation proof from the run context Values: `shared`, `isolated`. |
| `--isolation-proof` | `<PATH>` | Explicit homeboy/isolation-proof/v1 JSON proving destructive fuzz can run safely |
| `--allow-local-destructive-fuzz` | flag | Permit destructive fuzz to execute on the local controller instead of Lab |
| `--expect-metric` | `<METRIC=VALUE>` | Require a numeric metric emitted by the fuzz campaign to equal this value. Repeatable. Format: `--expect-metric metric_name=2` |
| `--action-model` | `<PATH>` | Generic action model contract JSON (`homeboy/fuzz-action-model/v1`) to include in the execution request |
| `--exploration-policy` | `<PATH>` | Generic exploration policy contract JSON (`homeboy/fuzz-exploration-policy/v1`) to include in the execution request |
| `--output-envelope` | `<PATH>` | Persist the result envelope JSON to this path |
| `--envelope-id` | `<ID>` | Stable envelope id. Defaults to --run-id, then the campaign id |

## `homeboy fuzz compare`

```sh
homeboy fuzz compare [OPTIONS] <BASELINE_ENVELOPE> <CANDIDATE_ENVELOPE>
```

Compare two persisted fuzz result envelopes

| Argument | Required | Description |
| --- | --- | --- |
| `<BASELINE_ENVELOPE>` | yes | Baseline fuzz result envelope JSON file |
| `<CANDIDATE_ENVELOPE>` | yes | Candidate fuzz result envelope JSON file |

| Option | Value | Description |
| --- | --- | --- |
| `--hotspot-policy` | `<HOTSPOT_POLICY>` | How relative hotspot regressions affect the blocking compare status Values: `advisory`, `blocking`, `off`. |

## `homeboy fuzz replay`

```sh
homeboy fuzz replay [OPTIONS] [ARTIFACT_OR_CASE] [ARGS]...
```

Resolve replay metadata for persisted fuzz cases

| Argument | Required | Description |
| --- | --- | --- |
| `[ARTIFACT_OR_CASE]` | no | Fuzz campaign/result envelope path, or a case id when --artifact is used |
| `[ARGS]...` | no | Additional arguments passed to the extension replay command |

| Option | Value | Description |
| --- | --- | --- |
| `--component` | `<ID>` | Component ID used to resolve the extension replay_command |
| `--path` | `<PATH>` | Override the component checkout path for replay command execution |
| `--rig` | `<RIG_ID>` | Resolve replay through a rig's component path and extension config |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--artifact` | `<PATH>` | Fuzz campaign or result envelope artifact to inspect for replay metadata |
| `--case-id` | `<ID>` | Case id to replay from the campaign/envelope artifact |
| `--run-id` | `<ID>` | Stable Homeboy run id associated with the persisted fuzz evidence |
| `--dry-run` | flag | Resolve replay metadata and command environment without executing replay_command |

## `homeboy fuzz minimize`

```sh
homeboy fuzz minimize [OPTIONS] [ARTIFACT_OR_CASE] [ARGS]...
```

Resolve minimization metadata for persisted fuzz cases

| Argument | Required | Description |
| --- | --- | --- |
| `[ARTIFACT_OR_CASE]` | no | Fuzz campaign/result envelope path, or a case id when --artifact is used |
| `[ARGS]...` | no | Additional arguments passed to the extension minimize command |

| Option | Value | Description |
| --- | --- | --- |
| `--component` | `<ID>` | Component ID used to resolve the extension minimize_command |
| `--path` | `<PATH>` | Override the component checkout path for minimize command execution |
| `--rig` | `<RIG_ID>` | Resolve minimization through a rig's component path and extension config |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--artifact` | `<PATH>` | Fuzz campaign or result envelope artifact to inspect for minimization metadata |
| `--case-id` | `<ID>` | Case id to minimize from the campaign/envelope artifact |
| `--run-id` | `<ID>` | Stable Homeboy run id associated with the persisted fuzz evidence |
| `--dry-run` | flag | Resolve minimization metadata and command environment without executing minimize_command |

## `homeboy fuzz inspect`

```sh
homeboy fuzz inspect [OPTIONS] <RUN_ID>
```

Print a compact fuzz failure diagnosis or the complete runner result

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Homeboy run id whose raw fuzz result should be inspected. Accepts the fuzz run id or the Lab runner-exec run id that offloaded it |

| Option | Value | Description |
| --- | --- | --- |
| `--raw` | flag | Print the complete result body as raw bytes/text |
| `--full` | flag | Print the complete parsed result body instead of the bounded diagnosis |
