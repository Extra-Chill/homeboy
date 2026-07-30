<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy runs` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/runs.md](../../../commands/runs.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy runs`

```sh
homeboy runs <COMMAND>
```

Inspect persisted observation runs and artifacts

| Subcommand | Summary |
| --- | --- |
| `homeboy runs list` | List persisted observation runs |
| `homeboy runs distribution` | Aggregate persisted run metadata |
| `homeboy runs latest-run` | Show the latest persisted observation run matching filters |
| `homeboy runs compare` | Compare selected metrics across persisted run history |
| `homeboy runs bench-compare` | Compare two persisted benchmark runs by exact run id |
| `homeboy runs fuzz-compare` | Compare two persisted fuzz runs by exact run id |
| `homeboy runs hotspots` | Aggregate hotspot rankings across persisted fuzz run artifacts |
| `homeboy runs reconcile` | Mark orphaned running observation records stale |
| `homeboy runs watch` | Block and stream a run's status until it reaches a terminal state, exiting with a code that reflects pass/fail. Works for attached and detached/offloaded runs |
| `homeboy runs cancel` | Cooperatively cancel a persisted foreground run before its next stage |
| `homeboy runs show` | Show one persisted observation run |
| `homeboy runs proof` | Show only the compact proof signals for one run: verdict, gate failures, and declared proof/scorecard signal fields. Full evidence stays behind `runs show --json` / `runs evidence` |
| `homeboy runs dossier` | Aggregate the actionable read-only dossier for one persisted run |
| `homeboy runs resume-plan` | Show a generic resume plan for a validation-progress run |
| `homeboy runs evidence` | Show stable evidence registry data for one run; start here for reviewer-facing evidence |
| `homeboy runs env` | Explain redacted Lab environment provenance for one run |
| `homeboy runs artifacts` | List artifacts recorded for one run |
| `homeboy runs artifact` | Retrieve or sync recorded run artifacts |
| `homeboy runs findings` | List findings recorded for one run |
| `homeboy runs finding` | Show one recorded finding |
| `homeboy runs latest-finding` | Show the latest finding from the latest run matching filters |
| `homeboy runs export` | Export observation records as an inspectable directory bundle |
| `homeboy runs import` | Import an observation bundle (default) or ingest GitHub Actions artifacts (`--from-gh-actions`) |
| `homeboy runs query` | Project JSONPath expressions over imported run artifact rows |
| `homeboy runs refs` | Emit stable run/artifact refs for matching runs |
| `homeboy runs resources` | Inspect resource lifecycle records from resource index files |
| `homeboy runs drift` | Window-based distribution drift over a JSONPath metric |
| `homeboy runs loop-sync` | Sync continuous-loop archive directories into observation artifacts |

## `homeboy runs list`

```sh
homeboy runs list [OPTIONS]
```

List persisted observation runs

| Option | Value | Description |
| --- | --- | --- |
| `--runner` | `<RUNNER>` | Query runs from a connected execution runner daemon |
| `--kind` | `<KIND>` | Run kind: bench, rig, trace, etc |
| `--component` | `<COMPONENT_ID>` | Component ID |
| `--rig` | `<RIG>` | Rig ID |
| `--scenario` | `<SCENARIO_ID>` | Benchmark scenario ID. Only applies to bench metadata |
| `--status` | `<STATUS>` | Run status |
| `--running` | flag | Show only in-flight runs. Shorthand for `--status running`; surfaces runs that could otherwise become ghosts |
| `--since` | `<SINCE>` | Only include runs started at or after this RFC-3339 timestamp (e.g. `2026-07-22T00:00:00Z`) or a relative age (`2d`, `6h`, `30m`) |
| `--until` | `<UNTIL>` | Only include runs started at or before this RFC-3339 timestamp or a relative age (`2d`, `6h`, `30m`) |
| `--id` | `<ID>` | Match runs whose persisted id or run-label contains this fragment |
| `--command-contains` | `<COMMAND_CONTAINS>` | Match runs whose command string contains this substring |
| `--correlation` | `<CORRELATION>` | Resolve controller run, runner job, and mirrored observation records that share this correlation/lineage fragment (matches persisted id, run-label, runner id, or job id) |
| `--include-mirrors` | flag | Show every underlying observation row, including runner-execution mirrors that are collapsed into one canonical row by default |
| `--limit` | `<LIMIT>` | Maximum runs to return |
| `--include-active-runner-jobs` | flag | Include active runner jobs from connected runner daemons |

## `homeboy runs distribution`

```sh
homeboy runs distribution [OPTIONS]
```

Aggregate persisted run metadata

| Option | Value | Description |
| --- | --- | --- |
| `--kind` | `<KIND>` | Run kind: bench, rig, trace, etc |
| `--component` | `<COMPONENT_ID>` | Component ID |
| `--rig` | `<RIG>` | Rig ID |
| `--scenario` | `<SCENARIO_ID>` | Benchmark scenario ID. Only applies to bench metadata |
| `--status` | `<STATUS>` | Run status |
| `--field` | `<FIELDS>` | Dot-separated metadata path to aggregate |
| `--limit` | `<LIMIT>` | Maximum runs to inspect before scenario filtering |

## `homeboy runs latest-run`

```sh
homeboy runs latest-run [OPTIONS]
```

Show the latest persisted observation run matching filters

| Option | Value | Description |
| --- | --- | --- |
| `--kind` | `<KIND>` | Run kind: bench, rig, trace, etc |
| `--component` | `<COMPONENT_ID>` | Component ID |
| `--rig` | `<RIG>` | Rig ID |
| `--status` | `<STATUS>` | Run status |

## `homeboy runs compare`

```sh
homeboy runs compare [OPTIONS]
```

Compare selected metrics across persisted run history

| Option | Value | Description |
| --- | --- | --- |
| `--kind` | `<KIND>` | Run kind: bench, rig, trace, etc |
| `--component` | `<COMPONENT_ID>` | Component ID |
| `--rig` | `<RIG>` | Rig ID |
| `--scenario` | `<SCENARIO_ID>` | Scenario ID for scenario-scoped metrics |
| `--status` | `<STATUS>` | Run status |
| `--metric` | `<METRICS>` | Metric to include. Repeat to compare multiple metrics |
| `--limit` | `<LIMIT>` | Maximum runs to inspect |
| `--format` | `<FORMAT>` | Output format Values: `table`, `json`. |

## `homeboy runs bench-compare`

```sh
homeboy runs bench-compare [OPTIONS]
```

Compare two persisted benchmark runs by exact run id

| Option | Value | Description |
| --- | --- | --- |
| `--from-run` | `<FROM_RUN>` | Earlier benchmark run ID |
| `--to-run` | `<TO_RUN>` | Later benchmark run ID |
| `--metric` | `<METRICS>` | Metric to include. Repeat to compare multiple metrics. Defaults to all shared numeric metrics |

## `homeboy runs fuzz-compare`

```sh
homeboy runs fuzz-compare [OPTIONS]
```

Compare two persisted fuzz runs by exact run id

| Option | Value | Description |
| --- | --- | --- |
| `--from-run` | `<RUN_ID>` | Baseline fuzz run id |
| `--to-run` | `<RUN_ID>` | Candidate fuzz run id |
| `--hotspot-policy` | `<HOTSPOT_POLICY>` | How relative hotspot regressions affect the blocking compare status Values: `advisory`, `blocking`, `off`. |

## `homeboy runs hotspots`

```sh
homeboy runs hotspots [OPTIONS] [RUN_ID]...
```

Aggregate hotspot rankings across persisted fuzz run artifacts

| Argument | Required | Description |
| --- | --- | --- |
| `[RUN_ID]...` | no | One or more persisted Homeboy run ids to inspect |

| Option | Value | Description |
| --- | --- | --- |
| `--baseline-run` | `<RUN_ID>` | Baseline run id for threshold-free cohort comparison |
| `--candidate-run` | `<RUN_ID>` | Candidate run id for threshold-free cohort comparison |
| `--limit` | `<LIMIT>` | Maximum ranked hotspots to return |

## `homeboy runs reconcile`

```sh
homeboy runs reconcile [OPTIONS]
```

Mark orphaned running observation records stale

| Option | Value | Description |
| --- | --- | --- |
| `--dry-run` | flag | Preview orphaned running records without mutating them |
| `--limit` | `<LIMIT>` | Maximum running records to inspect |

## `homeboy runs watch`

Aliases: `follow`, `tail`

```sh
homeboy runs watch [OPTIONS] <RUN_ID>
```

Block and stream a run's status until it reaches a terminal state, exiting with a code that reflects pass/fail. Works for attached and detached/offloaded runs

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Observation run id to watch until it reaches a terminal state |

| Option | Value | Description |
| --- | --- | --- |
| `--timeout` | `<TIMEOUT>` | Maximum time to wait before giving up (e.g. `30m`, `2h`, `7d`). Defaults to five minutes; use `--forever` for an intentional indefinite watch |
| `--forever` | flag | Keep watching without a time bound. This is explicit because streams are otherwise finite by default |
| `--interval` | `<INTERVAL>` | Delay between status polls (e.g. `2s`, `1m`) |
| `--notify` | flag | Emit a local completion notification when the run reaches a terminal state. Delivery resolves the run's installed extension transport, or an explicitly configured operations transport for route-less runs |

## `homeboy runs cancel`

```sh
homeboy runs cancel <RUN_ID>
```

Cooperatively cancel a persisted foreground run before its next stage

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

## `homeboy runs show`

```sh
homeboy runs show [OPTIONS] <RUN_ID>
```

Show one persisted observation run

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | flag | Print the full JSON output instead of the compact human summary. The compact summary surfaces status, key metadata, and artifact pointers with inspect commands; the full payload is unchanged and always available with this flag or via `--output <file>` |
| `-q`, `--field` | `<FIELD>` | JSONPath selector(s) projected over the run detail so callers extract only specific fields instead of the whole structure. Repeat or comma-separate. Rooted at the run detail, e.g. `-q '$.status'`, `-q '$.metadata.run_dir'` |

## `homeboy runs proof`

```sh
homeboy runs proof [OPTIONS] <RUN_ID>
```

Show only the compact proof signals for one run: verdict, gate failures, and declared proof/scorecard signal fields. Full evidence stays behind `runs show --json` / `runs evidence`

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | flag | Print the full JSON output instead of the compact human summary |

## `homeboy runs dossier`

```sh
homeboy runs dossier [OPTIONS] <RUN_ID>
```

Aggregate the actionable read-only dossier for one persisted run

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | flag | Print the full JSON output instead of the compact human dossier |

## `homeboy runs resume-plan`

```sh
homeboy runs resume-plan <RUN_ID>
```

Show a generic resume plan for a validation-progress run

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

## `homeboy runs evidence`

```sh
homeboy runs evidence <RUN_ID>
```

Show stable evidence registry data for one run; start here for reviewer-facing evidence

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

## `homeboy runs env`

```sh
homeboy runs env <RUN_ID>
```

Explain redacted Lab environment provenance for one run

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

## `homeboy runs artifacts`

```sh
homeboy runs artifacts [OPTIONS] <RUN_ID>
```

List artifacts recorded for one run

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Observation run id that owns the artifacts |

| Option | Value | Description |
| --- | --- | --- |
| `--runner` | `<RUNNER>` | Query artifacts from a connected execution runner daemon |
| `--pull` | flag | Pull runner/remote artifact bytes to the operator-local artifact root so the completed run is self-contained. Best-effort and per-artifact: the listing still prints, and each artifact reports a pull status |
| `--pull-dir` | `<PULL_DIR>` | Optional directory to write pulled artifact bytes into. Defaults to a run-scoped path under the operator-local artifact root |

## `homeboy runs artifact`

```sh
homeboy runs artifact <COMMAND>
```

Retrieve or sync recorded run artifacts

| Subcommand | Summary |
| --- | --- |
| `homeboy runs artifact attach` | Attach an existing runner-side output file to a persisted run |
| `homeboy runs artifact get` | Copy a recorded file artifact to a local path |
| `homeboy runs artifact preview` | Serve a recorded directory artifact with a local static preview URL |
| `homeboy runs artifact capture` | Capture generated HTML entrypoint screenshots from a recorded directory artifact |
| `homeboy runs artifact cleanup-downloads` | Plan or delete locally cached runner artifact downloads |
| `homeboy runs artifact cleanup-persisted` | Plan or delete persisted local run artifacts and their database records |
| `homeboy runs artifact postprocess` | Run a generic artifact postprocess plan over persisted artifact roots |

## `homeboy runs artifact attach`

```sh
homeboy runs artifact attach [OPTIONS] <RUN_ID>
```

Attach an existing runner-side output file to a persisted run

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Observation run id that should own the attached artifact |

| Option | Value | Description |
| --- | --- | --- |
| `--runner` | `<RUNNER>` | Runner ID that can read the path |
| `--path` | `<PATH>` | Absolute runner-side file path under an allowed workspace/output root |
| `--name` | `<NAME>` | Artifact kind/name to record in the observation store |

## `homeboy runs artifact get`

```sh
homeboy runs artifact get [OPTIONS] <RUN_ID> <ARTIFACT_ID>
```

Copy a recorded file artifact to a local path

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Observation run id that owns the artifact |
| `<ARTIFACT_ID>` | yes | Artifact id/path token from `homeboy runs artifacts <run-id>` |

| Option | Value | Description |
| --- | --- | --- |
| `--runner` | `<RUNNER>` | Pull the artifact from a connected execution runner daemon |
| `-o`, `--output` | `<OUTPUT>` | Destination file path. Defaults to the recorded artifact filename |
| `-q`, `--field` | `<FIELD>` | JSONPath selector(s) projected over the artifact-get result so callers extract only specific fields (e.g. `sha256`, `output_path`) instead of the whole structure. Repeat or comma-separate. Field selection still writes the artifact bytes when `--output` is set. Example: `-q '$.sha256'`, `-q '$.output_path'` |

## `homeboy runs artifact preview`

```sh
homeboy runs artifact preview [OPTIONS] <RUN_ID> <ARTIFACT_ID>
```

Serve a recorded directory artifact with a local static preview URL

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Observation run id that owns the artifact |
| `<ARTIFACT_ID>` | yes | Directory artifact id/path token from `homeboy runs artifacts <run-id>` |

| Option | Value | Description |
| --- | --- | --- |
| `--port` | `<PORT>` | Local loopback port. Defaults to an available ephemeral port |

## `homeboy runs artifact capture`

```sh
homeboy runs artifact capture [OPTIONS] <RUN_ID> <ARTIFACT_ID>
```

Capture generated HTML entrypoint screenshots from a recorded directory artifact

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Observation run id that owns the artifact |
| `<ARTIFACT_ID>` | yes | Directory artifact id/path token from `homeboy runs artifacts <run-id>` |

| Option | Value | Description |
| --- | --- | --- |
| `--entrypoint` | `<ENTRYPOINTS>` | HTML path inside the directory artifact. Repeat for multiple pages |
| `--output-dir` | `<OUTPUT_DIR>` | Directory where screenshots and capture-manifest.json should be written |
| `--port` | `<PORT>` | Local loopback port. Defaults to an available ephemeral port |
| `--viewport-width` | `<VIEWPORT_WIDTH>` | Browser viewport width in CSS pixels |
| `--viewport-height` | `<VIEWPORT_HEIGHT>` | Browser viewport height in CSS pixels |

## `homeboy runs artifact cleanup-downloads`

```sh
homeboy runs artifact cleanup-downloads [OPTIONS]
```

Plan or delete locally cached runner artifact downloads

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Delete the planned cached downloads. Without this flag, only reports the plan |
| `--runner` | `<RUNNER>` | Limit cleanup to one runner id under the local runner artifact cache |
| `--run-id` | `<RUN_ID>` | Limit cleanup to one run id. Requires --runner |

## `homeboy runs artifact cleanup-persisted`

```sh
homeboy runs artifact cleanup-persisted [OPTIONS]
```

Plan or delete persisted local run artifacts and their database records

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Delete planned artifact files/directories and their DB rows. Without this flag, only reports the plan |
| `--older-than-days` | `<OLDER_THAN_DAYS>` | Only include artifacts older than this many days. Defaults to the configured `retention.terminal_run_days` |
| `--run-id` | `<RUN_ID>` | Limit cleanup to one run id |
| `--kind` | `<KIND>` | Limit cleanup to one artifact kind |
| `--type` | `<ARTIFACT_TYPE>` | Limit cleanup to one artifact type (`file` or `directory`) |
| `--run-kind` | `<RUN_KIND>` | Limit cleanup to one run kind (`bench`, `trace`, etc.) |
| `--component` | `<COMPONENT_ID>` | Limit cleanup to one component id |
| `--limit` | `<LIMIT>` | Maximum artifact rows to inspect in one invocation. Defaults to the configured `retention.limit` |

## `homeboy runs artifact postprocess`

```sh
homeboy runs artifact postprocess [OPTIONS] <PLAN>
```

Run a generic artifact postprocess plan over persisted artifact roots

| Argument | Required | Description |
| --- | --- | --- |
| `<PLAN>` | yes | Artifact postprocess plan JSON file, @file spec, or - for stdin |

| Option | Value | Description |
| --- | --- | --- |
| `--artifact-root-id` | `<ID>` | Artifact root id from the plan to use as HOMEBOY_ARTIFACT_POSTPROCESS_ARTIFACT_ROOT |
| `--input-root-id` | `<ID>` | Optional artifact root id from the plan to expose as ${run.input} |
| `--result` | `<PATH>` | Write the bare artifact-postprocess result contract to this path |

## `homeboy runs findings`

```sh
homeboy runs findings [OPTIONS] [RUN_ID] [COMMAND]
```

List findings recorded for one run

| Argument | Required | Description |
| --- | --- | --- |
| `[RUN_ID]` | no | Observation run ID |

| Option | Value | Description |
| --- | --- | --- |
| `--tool` | `<TOOL>` | Finding tool, for example lint |
| `--file` | `<FILE>` | Finding file path |
| `--fingerprint` | `<FINGERPRINT>` | Finding fingerprint |
| `--limit` | `<LIMIT>` | Maximum findings to return |

| Subcommand | Summary |
| --- | --- |
| `homeboy runs findings reconcile` | Reconcile a finding stream against an issue tracker |
| `homeboy runs findings reconcile-run` | Reconcile all structured command outputs in one CI run |
| `homeboy runs findings build` | Convert native command output into the canonical reconcile input shape |

## `homeboy runs findings reconcile`

```sh
homeboy runs findings reconcile [OPTIONS] <COMPONENT_ID>
```

Reconcile a finding stream against an issue tracker.

Reads structured findings (from `homeboy review audit --json-summary` or `homeboy review lint --json` or any equivalent), inspects open and closed issues on the tracker, and produces a deterministic plan: file new, update, close, dedupe, or skip per category.

Defaults to dry-run; pass `--apply` to actually call the tracker.

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component ID. Tracker repo is resolved from this component's `remote_url` (or git remote, when --path is set) |

| Option | Value | Description |
| --- | --- | --- |
| `--tracker` | `<URI>` | Tracker URI. Currently only `github://owner/repo` is supported. When omitted, defaults to the component's GitHub remote — the common case |
| `--findings` | `<PATH>` | Path to a JSON findings file. Use `-` to read from stdin. The file's shape: |
| `--from-output` | `<COMMAND=PATH>` | Native Homeboy command output to normalize before reconcile. Repeatable as `--from-output audit=/tmp/audit.json` |
| `--run-url` | `<URL>` | Optional run URL appended to generated issue bodies when using `--from-output` |
| `--no-refresh-closed` | flag | Don't refresh the body of closed-not_planned issues with the latest finding count. Default is to refresh (so the closed issue stays useful as a "current state" reference) |
| `--list-limit` | `<LIST_LIMIT>` | Cap the number of issues fetched from the tracker for dedup analysis. Defaults to 200 — high enough for normal repos, but avoids paginating the entire tracker |
| `--apply` | flag | Actually perform the reconcile actions. Default is dry-run |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json (CI runners, ad-hoc clones) |

## `homeboy runs findings reconcile-run`

```sh
homeboy runs findings reconcile-run [OPTIONS] <COMPONENT_ID>
```

Reconcile all structured command outputs in one CI run.

Discovers `<command>.json` files in an output directory, runs the existing per-command reconcile pipeline, and returns aggregate totals suitable for GitHub Action consumption.

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Default component ID. Per-output component metadata overrides this when present in the command JSON |

| Option | Value | Description |
| --- | --- | --- |
| `--output-dir` | `<DIR>` | Directory containing structured command outputs such as `audit.json`, `lint.json`, and `test.json`. Defaults to HOMEBOY_OUTPUT_DIR when omitted |
| `--commands` | `<COMMANDS>` | Comma-separated command list to inspect in the output directory |
| `--run-url` | `<URL>` | Optional run URL appended to generated issue bodies |
| `--no-refresh-closed` | flag | Don't refresh the body of closed-not_planned issues with the latest finding count |
| `--list-limit` | `<LIST_LIMIT>` | Cap the number of issues fetched from the tracker for dedup analysis per command |
| `--apply` | flag | Actually perform the reconcile actions. Default is dry-run |
| `--path` | `<PATH>` | Workspace path to discover the component from a portable homeboy.json (CI runners, ad-hoc clones) |

## `homeboy runs findings build`

```sh
homeboy runs findings build [OPTIONS]
```

Convert native command output into the canonical reconcile input shape

| Option | Value | Description |
| --- | --- | --- |
| `--from-output` | `<COMMAND=PATH>` | Native Homeboy command output to normalize. Repeatable as `--from-output audit=/tmp/audit.json` |
| `--run-url` | `<URL>` | Optional run URL appended to generated issue bodies |

## `homeboy runs finding`

```sh
homeboy runs finding <FINDING_ID>
```

Show one recorded finding

| Argument | Required | Description |
| --- | --- | --- |
| `<FINDING_ID>` | yes | _no help text_ |

## `homeboy runs latest-finding`

```sh
homeboy runs latest-finding [OPTIONS]
```

Show the latest finding from the latest run matching filters

| Option | Value | Description |
| --- | --- | --- |
| `--kind` | `<KIND>` | Run kind: bench, rig, trace, etc |
| `--component` | `<COMPONENT_ID>` | Component ID |
| `--rig` | `<RIG>` | Rig ID |
| `--status` | `<STATUS>` | Run status |
| `--tool` | `<TOOL>` | Finding tool, for example lint |
| `--file` | `<FILE>` | Finding file path |

## `homeboy runs export`

```sh
homeboy runs export [OPTIONS]
```

Export observation records as an inspectable directory bundle

| Option | Value | Description |
| --- | --- | --- |
| `--run` | `<RUN>` | Export one run by id |
| `--since` | `<SINCE>` | Export runs started within a duration, e.g. 24h, 7d, 30m |
| `--output` | `<DIR>` | Output bundle directory. Zip output is intentionally out of scope for v1 |

## `homeboy runs import`

```sh
homeboy runs import [OPTIONS] [INPUT]
```

Import an observation bundle (default) or ingest GitHub Actions artifacts (`--from-gh-actions`)

| Argument | Required | Description |
| --- | --- | --- |
| `[INPUT]` | no | Bundle directory produced by `homeboy runs export`. Required when not using `--from-gh-actions`. Mutually exclusive with `--from-gh-actions` |

| Option | Value | Description |
| --- | --- | --- |
| `--from-gh-actions` | flag | Ingest artifacts directly from GitHub Actions instead of from a portable bundle directory. When set, `--component`, `--repo`, `--artifact-glob`, and one of `--workflow` or `--run-id` are required |
| `--component` | `<COMPONENT_ID>` | Component ID to stamp on imported runs (gh-actions mode) |
| `--repo` | `<REPO>` | `owner/repo` form (gh-actions mode) |
| `--workflow` | `<WORKFLOW>` | Workflow filename or display name (gh-actions mode) |
| `--run-id` | `<RUN_ID>` | Exact GitHub Actions run id (gh-actions mode) |
| `--artifact-glob` | `<ARTIFACT_GLOB>` | Glob filter for artifact names (gh-actions mode). Examples: `'design-distribution-*'`, `'*.json'` |
| `--since` | `<SINCE>` | Restrict the gh-actions ingest window (e.g. 24h, 7d, 30d) |
| `--limit` | `<LIMIT>` | Maximum runs to inspect per import call (gh-actions mode) |

## `homeboy runs query`

```sh
homeboy runs query [OPTIONS]
```

Project JSONPath expressions over imported run artifact rows

| Option | Value | Description |
| --- | --- | --- |
| `--component` | `<COMPONENT_ID>` | Component ID (matches the synthetic Homeboy run's component_id) |
| `--kind` | `<KIND>` | Run kind (e.g. `gh-actions`). Defaults to all kinds |
| `--since` | `<SINCE>` | Restrict to runs started within this duration (e.g. 24h, 7d) |
| `--select` | `<SELECT>` | One or more JSONPath expressions to project. Comma-separated. Example: `--select '$.theme,$.fonts[*].family'` |
| `--group-by` | `<GROUP_BY>` | Optional JSONPath expression to group by |
| `--count` | flag | When set with `--group-by`, emit `(group, count)` instead of full rows |
| `--format` | `<FORMAT>` | Output format Values: `json`, `table`, `csv`. |
| `--limit` | `<LIMIT>` | Maximum runs to inspect |

## `homeboy runs refs`

```sh
homeboy runs refs [OPTIONS]
```

Emit stable run/artifact refs for matching runs

| Option | Value | Description |
| --- | --- | --- |
| `--component` | `<COMPONENT_ID>` | Component ID |
| `--kind` | `<KIND>` | Run kind: bench, rig, trace, gh-actions, etc |
| `--rig` | `<RIG>` | Rig ID |
| `--status` | `<STATUS>` | Run status |
| `--since` | `<SINCE>` | Restrict to runs started within this duration (e.g. 24h, 7d) |
| `--limit` | `<LIMIT>` | Maximum runs to inspect |
| `--artifact-kind` | `<ARTIFACT_KINDS>` | Restrict artifact refs to these artifact kinds. Repeatable |
| `--aggregate-artifact-kind` | `<AGGREGATE_ARTIFACT_KINDS>` | Treat these artifact kinds as aggregate refs in addition to the default schema-blind aggregate detector. Repeatable |

## `homeboy runs resources`

```sh
homeboy runs resources [OPTIONS]
```

Inspect resource lifecycle records from resource index files

| Option | Value | Description |
| --- | --- | --- |
| `--file` | `<PATH>` | Resource lifecycle index JSON file. Repeatable. Defaults to the local observation store |
| `--sample` | flag | Emit a contract-valid sample index instead of reading files or the observation store |
| `--run-id` | `<RUN_ID>` | Include only resources owned by this run id |
| `--owner` | `<OWNER>` | Include only resources owned by this resource owner |
| `--actionable` | flag | Include only records requiring operator/orchestrator attention |
| `--cleanup-eligible` | flag | Include only records eligible for cleanup orchestration |
| `--cleanup-plan` | flag | Emit cleanup planning data for matching resources. This is read-only unless --apply is also passed |
| `--apply` | flag | Delete apply-intended cleanup-eligible resources. Requires --cleanup-root and remains bounded by --limit |
| `--cleanup-root` | `<PATH>` | Root directory that cleanup candidates must canonicalize under before apply can delete them |
| `--limit` | `<LIMIT>` | Maximum cleanup candidates to include in the plan/apply page |
| `--cleanup-operation` | `<CLEANUP_OPERATION>` | Cleanup operation used by apply. Delete removes files, symlinks, or directories Values: `delete`. |

## `homeboy runs drift`

```sh
homeboy runs drift [OPTIONS]
```

Window-based distribution drift over a JSONPath metric

| Option | Value | Description |
| --- | --- | --- |
| `--component` | `<COMPONENT_ID>` | Component ID (matches the synthetic Homeboy run's component_id) |
| `--kind` | `<KIND>` | Run kind (e.g. `gh-actions`) |
| `--metric` | `<METRIC>` | JSONPath expression naming the metric to track. Example: `--metric '$.theme'` or `--metric '$.fonts[*].family'` |
| `--window` | `<WINDOW>` | Window duration to evaluate (e.g. 24h, 7d) |
| `--threshold` | `<THRESHOLD>` | Share threshold in `0.0..=1.0`. Values whose share of the window exceeds this threshold are flagged as `dominant=true`. The default (0.0) reports every value |
| `--baseline` | `<BASELINE>` | Optional baseline window (e.g. 30d) compared against `--window` |
| `--format` | `<FORMAT>` | Output format Values: `json`, `table`. |

## `homeboy runs loop-sync`

```sh
homeboy runs loop-sync [OPTIONS] <ARCHIVE_ROOT>
```

Sync continuous-loop archive directories into observation artifacts

| Argument | Required | Description |
| --- | --- | --- |
| `<ARCHIVE_ROOT>` | yes | Local directory containing copied remote loop archives |

| Option | Value | Description |
| --- | --- | --- |
| `--component` | `<COMPONENT_ID>` | Optional component label for filtering the resulting observation run |
| `--rig` | `<RIG>` | Optional rig/loop label for filtering the resulting observation run |
| `--label` | `<LABELS>` | Optional free-form labels recorded in run metadata |
| `--stale-after-minutes` | `<STALE_AFTER_MINUTES>` | Mark heartbeat/session files stale after this many minutes |
| `--retention-days` | `<RETENTION_DAYS>` | Retention budget used for reporting old archive candidates |
| `--patch-limit` | `<PATCH_LIMIT>` | Maximum ranked patch candidates to include in triage output |
| `--dry-run` | flag | Inspect and triage without writing observation runs or artifacts |
