<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy trace` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/trace.md](../../../commands/trace.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy trace`

```sh
homeboy trace [OPTIONS] [COMPONENT] [SCENARIO] [AFTER_JSON]
```

Capture black-box behavioral traces for a component

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |
| `[SCENARIO]` | no | Scenario ID to run, or `list` to discover available scenarios |
| `[AFTER_JSON]` | no | After aggregate JSON when running `homeboy trace compare before.json after.json` |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--component` | `<COMPONENT_ID>` | Target component for command-shaped trace modes like `compare-variant` and `compare-bundle` |
| `--scenario` | `<SCENARIO_ID>` | Scenario ID or comma-separated scenario list for command-shaped trace modes like `compare-variant` and `compare-bundle` |
| `--baseline-target` | `<PATH_OR_REF>` | Baseline path or git ref for `homeboy trace compare COMPONENT SCENARIO` |
| `--candidate` | `<PATH_OR_REF>` | Candidate path or git ref for `homeboy trace compare COMPONENT SCENARIO` |
| `--rig` | `<RIG_ID>` | Run trace against a rig-pinned component path after `rig check` passes |
| `--profile` | `<PROFILE_ID>` | Use a named trace profile declared by a rig |
| `--profiles` | flag | With `trace list`, list named trace profiles instead of scenarios |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--secret-env` | `<NAME>` | Secret environment variable name to hydrate for the trace runner. Repeatable |
| `--json-summary` | flag | Print compact machine-readable summary |
| `--report` | `<REPORT>` | Render a Markdown trace report instead of the JSON envelope Values: `markdown`. |
| `--experiment` | `<NAME>` | Bundle trace compare inputs, output, report, and overlay metadata under .homeboy/experiments/NAME |
| `--repeat` | `<N>` | Run the same trace scenario multiple times |
| `--aggregate` | `<AGGREGATE>` | Aggregate repeated trace output Values: `spans`. |
| `--schedule` | `<SCHEDULE>` | Run order for repeated trace executions Values: `grouped`, `interleaved`. |
| `--focus-span` | `<SPAN_ID>` | Highlight a span in aggregate and compare reports. Repeatable |
| `--metric-guardrail` | `<SPEC>` | Compare scalar metrics with `METRIC[.min\|.median\|.max]:POLICY[:VALUE]`. Repeatable |
| `--span` | `<ID:FROM:TO>` | Add a span definition as `id:source.event:source.event` |
| `--phase` | `<[LABEL:]SOURCE.EVENT>` | Add an ordered phase milestone as `[label:]source.event` |
| `--attach` | `<KIND:TARGET>` | Observe an already-running local target without managing its lifecycle. Repeatable |
| `--phase-preset` | `<NAME>` | Use a named phase preset declared by the selected rig/workload |
| `--baseline` | flag | Persist the current run as the new baseline |
| `--ignore-baseline` | flag | Skip baseline comparison for this run |
| `--ratchet` | flag | Auto-update the baseline when the current run improves on it |
| `--regression-threshold` | `<PERCENT>` | Span regression tolerance as a percentage |
| `--regression-min-delta-ms` | `<MS>` | Minimum span slowdown in milliseconds before a regression can fail |
| `--overlay` | `<PATCH_FILE>` | Apply a patch file for this trace run, then reverse it afterward |
| `--variant` | `<NAME>` | Apply a named trace variant declared by the selected rig/workload |
| `--matrix` | `<MATRIX>` | Expand variants for `trace compare-variant` Values: `none`, `single`, `cumulative`. |
| `--axis` | `<NAME=VALUE[,VALUE...]>` | Add a scenario matrix axis as `name=value1,value2`. Repeatable |
| `--output-dir` | `<DIR>` | Directory where trace matrix and compare bundle modes write aggregate, compare, cell, and summary artifacts |
| `--visual-compare` | flag | Run visual screenshot comparisons for trace compare browser artifacts |
| `--visual-artifacts-dir` | `<DIR>` | Directory where visual compare artifacts should be written |
| `--visual-compare-provider` | `<COMMAND>` | Executable implementing the generic Homeboy visual compare provider contract |
| `--visual-provider-arg` | `<ARG>` | Extra argument forwarded to the visual compare provider before the input JSON path |
| `--visual-threshold` | `<RATIO>` | Visual mismatch threshold forwarded to the visual compare provider |
| `--keep-overlay` | flag | Leave overlay changes in place after the trace run |
| `--canonical` | flag | Require canonical evidence. This is the default; retained for explicit command logs |
| `--allow-local-toolchain` | flag | Allow intentionally local/development evidence. The output is marked non-canonical |
| `--stale` | flag | Clean only stale trace overlay locks |
| `--force` | flag | Remove stale trace overlay locks even when touched files are dirty |

