<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy report` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/report.md](../../../commands/report.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy report`

```sh
homeboy report <COMMAND>
```

Render reports from Homeboy structured output artifacts

| Subcommand | Summary |
| --- | --- |
| `homeboy report failure-digest` | Render a markdown failure digest from Homeboy command output JSON files |
| `homeboy report performance-digest` | Render a generic performance digest from Homeboy run artifacts |
| `homeboy report bench-coverage` | Report list-only benchmark coverage for hot command paths |
| `homeboy report browser-evidence-compare` | Compare before/after browser evidence artifact sets |
| `homeboy report matrix-artifacts` | Summarize matrix-style run artifacts and finding packets |
| `homeboy report compare` | Compare structured matrix/report artifacts |

## `homeboy report failure-digest`

```sh
homeboy report failure-digest [OPTIONS]
```

Render a markdown failure digest from Homeboy command output JSON files

| Option | Value | Description |
| --- | --- | --- |
| `--output-dir` | `<DIR>` | Directory containing audit.json, lint.json, test.json, etc |
| `--results` | `<JSON>` | Results JSON, e.g. '{"audit":"fail","lint":"pass"}' (supports @file) |
| `--run-url` | `<URL>` | Workflow run URL used as the fallback full-log link |
| `--tooling-json` | `<JSON_OR_FILE>` | Optional tooling metadata JSON file (supports @file) |
| `--commands` | `<CSV>` | Commands in this run, used to derive default autofix candidates |
| `--autofix-commands` | `<CSV>` | Commands with autofix support. Defaults to failed audit/lint/test commands |
| `--autofix-enabled` | flag | Whether automated fixes are enabled for this run |
| `--autofix-attempted` | flag | Whether automated fixes were already attempted in this run |
| `--format` | `<FORMAT>` | Output format. Markdown is the only supported report format for now Values: `markdown`. |

## `homeboy report performance-digest`

```sh
homeboy report performance-digest [OPTIONS]
```

Render a generic performance digest from Homeboy run artifacts

| Option | Value | Description |
| --- | --- | --- |
| `--output-dir` | `<DIR>` | Directory containing Homeboy run artifacts such as resource-summary.json and bench.json |
| `--metadata-json` | `<JSON_OR_FILE>` | Optional run metadata JSON, e.g. observation metadata or a status file (supports @file) |
| `--run-url` | `<URL>` | Workflow run URL used as the fallback full-log link |
| `--min-samples` | `<MIN_SAMPLES>` | Minimum run count for baseline health checks |
| `--max-cv-pct` | `<MAX_CV_PCT>` | Maximum coefficient of variation percentage before a baseline is considered noisy |
| `--format` | `<FORMAT>` | Output format. Markdown is the only direct-render report format for now Values: `markdown`. |

## `homeboy report bench-coverage`

```sh
homeboy report bench-coverage [OPTIONS] [COMPONENT]
```

Report list-only benchmark coverage for hot command paths

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--all` | flag | Inspect every registered component instead of the selected component |
| `--format` | `<FORMAT>` | Output format Values: `markdown`, `json`. |

## `homeboy report browser-evidence-compare`

```sh
homeboy report browser-evidence-compare [OPTIONS]
```

Compare before/after browser evidence artifact sets

| Option | Value | Description |
| --- | --- | --- |
| `--baseline-dir` | `<DIR>` | Directory containing baseline browser evidence JSON artifacts |
| `--candidate-dir` | `<DIR>` | Directory containing candidate browser evidence JSON artifacts |
| `--baseline-label` | `<BASELINE_LABEL>` | Label for the baseline artifact set |
| `--candidate-label` | `<CANDIDATE_LABEL>` | Label for the candidate artifact set |
| `--include-local-paths` | flag | Include local filesystem paths in Markdown output. By default Markdown only uses relative artifact names and URLs |
| `--format` | `<FORMAT>` | Output format. Markdown is direct-rendered; JSON uses the normal command envelope Values: `markdown`, `json`. |
| `--visual-compare` | flag | Run visual screenshot comparisons through a declared visual compare provider |
| `--visual-artifacts-dir` | `<DIR>` | Directory where visual compare artifacts should be written |
| `--visual-compare-provider` | `<COMMAND>` | Executable implementing the generic Homeboy visual compare provider contract |
| `--visual-provider-arg` | `<ARG>` | Extra argument forwarded to the visual compare provider before the input JSON path |
| `--visual-threshold` | `<RATIO>` | Visual mismatch threshold forwarded to the visual compare provider |

## `homeboy report matrix-artifacts`

```sh
homeboy report matrix-artifacts [OPTIONS] <RUN_ID>
```

Summarize matrix-style run artifacts and finding packets

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Observation run ID or the human `--run-id` launch label to summarize |

| Option | Value | Description |
| --- | --- | --- |
| `--format` | `<FORMAT>` | Output format: json or markdown |

## `homeboy report compare`

```sh
homeboy report compare [OPTIONS]
```

Compare structured matrix/report artifacts

| Option | Value | Description |
| --- | --- | --- |
| `--old` | `<RUN_OR_ARTIFACT>` | Baseline artifact input: local JSON path, run id, or run:artifact / run/artifact ref |
| `--new` | `<RUN_OR_ARTIFACT>` | Candidate artifact input: local JSON path, run id, or run:artifact / run/artifact ref |
| `--format` | `<FORMAT>` | Output format Values: `markdown`, `json`. |

