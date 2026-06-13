# homeboy trace

Capture black-box behavioral traces for a component. Trace runners write a JSON evidence envelope plus optional artifacts under the Homeboy run directory.

## Usage

```sh
homeboy trace <component> <scenario>
homeboy trace <component> list
homeboy trace <component> <scenario> --rig <rig-id>
homeboy trace <component> <scenario> --json-summary
homeboy trace <component> <scenario> --span submit_to_cli:ui.submit:cli.start
homeboy trace <component> <scenario> --span running:renderer.site_event_received[data.running=true]:renderer.dom_status_running_seen
homeboy trace <component> <scenario> --phase submit:ui.submit --phase cli:cli.start --phase ready:server.ready
homeboy trace <component> <scenario> --rig <rig-id> --phase-preset create-site
homeboy trace matrix <component> <scenario> --axis viewport=desktop,mobile --axis ece_locations=product,none
homeboy trace <component> <scenario> --runs 5 --aggregate spans --schedule interleaved
homeboy trace <component> <scenario> --attach logfile:/tmp/service.log --attach pid:1234
homeboy trace compare before.json after.json --focus-span phase.wp_boot_start_to_wp_boot_ready
homeboy trace compare <component> <scenario> --baseline-target develop --candidate HEAD --runs 5 --report markdown
homeboy trace compare-variant --rig studio --scenario studio-app-create-site --runs 5 --overlay overlays/change.patch --output-dir .homeboy/experiments/change
homeboy trace <component> <scenario> --report=markdown
homeboy trace <component> <scenario> --baseline
homeboy trace <component> <scenario> --ratchet
homeboy trace --profile studio-window-close
homeboy trace list --profiles
```

## Profiles

Trace profiles are named shortcuts declared in rig specs. They resolve to the same runner contract as a normal `homeboy trace` invocation; Homeboy fills unset CLI fields from the profile before resolving the component, rig workloads, overlays, variants, and settings.

```jsonc
{
  "trace_profiles": {
    "studio-window-close": {
      "component": "studio",
      "scenario": "close-window-running-site",
      "settings": {
        "window_title": "Studio",
        "retry_count": 2
      },
      "overlays": ["overlays/window-lifecycle.patch"],
      "variants": ["fresh-install-mode"]
    }
  }
}
```

Run the profile directly:

```sh
homeboy trace --profile studio-window-close
```

When `--rig` is omitted, Homeboy searches installed rig specs and requires the profile id to be unique. Pass `--rig <rig-id>` to scope lookup when multiple rigs declare the same profile id. CLI flags override profile fields, so `homeboy trace --profile studio-window-close --scenario close-window-retry` keeps the profile's component/settings while replacing the scenario.

List installed profiles:

```sh
homeboy trace list --profiles
homeboy trace list --profiles --rig studio
```

JSON run, summary, and aggregate outputs include a `profile` object with the resolved profile id, rig id, component, scenario, overlays, variants, and settings used for the invocation.

Trace dependency preflight rejects stale or dirty dependency checkouts before running the expensive workflow so stale local dependencies cannot produce misleading evidence.

## Public Preview Asset Gates

Trace workloads can declare `public_preview.required_asset_paths` for serial asset
preflight and `public_preview.asset_fanout` for concurrent browser/static asset
stress proof. `asset_fanout` starts after the public preview URL is available and
before the trace runner begins, then fetches every configured asset path through
the public origin with bounded concurrency. Any aborted request, timeout,
non-2xx response, or optional body-content mismatch fails the trace before it can
produce misleading browser evidence.

Use this gate when a trace depends on a public preview serving a burst of static
assets reliably:

```jsonc
{
  "public_preview": {
    "local_origin": "http://127.0.0.1:49823",
    "public_origin": "https://preview.example.test",
    "require_https": true,
    "asset_fanout": {
      "asset_paths": [
        "/assets/app.js?ver=1",
        "/assets/app.css?ver=1",
        "/assets/vendor.js?ver=1"
      ],
      "concurrency": 16,
      "repeat_count": 3
    }
  }
}
```

The emitted preview metadata includes `asset_fanout.schema =
homeboy/preview-asset-fanout/v1`, expected/client request totals, status counts,
failure buckets, and request rows. Native tunnel integrations may also fill
ingress and local-origin request counts when those counters are available.

Keep core fanout proof generic. WP Codebox, WooCommerce, or other product-specific
asset lists should live in their owning rig or extension and consume this
contract.

## Baseline/Candidate Compare

`homeboy trace compare <component> <scenario>` can run the same trace scenario against two local paths or git refs, aggregate the span timings, write JSON artifacts, and render a Markdown summary. This is the first-class A/B browser proof workflow for trace rigs: pass baseline and candidate targets, choose `--runs`, and Homeboy preserves per-run artifacts while producing reviewer-ready span and browser evidence tables.

```sh
homeboy trace compare woocommerce-gateway-stripe ece-product-page-waterfall \
  --baseline-target develop \
  --candidate HEAD \
  --runs 5 \
  --visual-compare \
  --visual-compare-provider <COMMAND> \
  --report markdown
```

`--baseline-target` accepts an existing path or a git ref in the resolved component checkout. The flag is intentionally named `--baseline-target` because `--baseline` already saves a trace baseline for the baseline engine. `--candidate` accepts the same path-or-ref shape. Ref targets are checked out into temporary detached git worktrees for the run and removed afterward.

The compare command writes `baseline.aggregate.json`, `candidate.aggregate.json`, `compare.json`, and `summary.md` under `.homeboy/trace-compare/<scenario>-<timestamp>` unless `--output-dir` is provided. The JSON comparison includes target labels, git SHAs when available, pass/fail status for both sides, artifact paths, span deltas, percentage deltas, focus-span status, guardrail status, and missing metrics as `null`/omitted values rather than invented numbers.

Target compares run baseline and candidate through the same scheduler. Use `--schedule interleaved` to alternate `baseline, candidate, baseline, candidate` for the configured repetition count, or `--schedule grouped` to run one side then the other. `compare.json` and `summary.md` include a `proof_run_order` / `A/B Run Matrix` section with each run's group, iteration, status, exit code, raw trace artifact path, and failure message when a run fails. Failed runs are retained in the aggregate and comparison instead of being silently dropped.

When run artifacts contain browser evidence JSON, target compare also attaches `browser_proof` to `compare.json` and appends a `Browser Evidence Comparison` section to `summary.md`. The browser proof aggregates promoted browser metrics such as LCP, ready time, DOM lifecycle timings, request counts, console errors, page errors, assertion deltas, and artifact references across baseline/candidate samples. Profile and matrix labels emitted by the evidence are preserved so throttled, synthetic, viewport, or scenario-profiled runs remain distinguishable in review. Treat throttled or synthetic timing labels as relative proof data unless the underlying profile says otherwise.

Pass `--visual-compare` when the browser evidence includes screenshot artifacts
and a visual compare provider is available. Homeboy delegates visual diff work to
the executable named by `--visual-compare-provider`, writes visual artifacts under
`--visual-artifacts-dir` or the trace compare output directory, and records
normalized mismatch metrics plus source/candidate/diff artifact refs in
`browser_proof.report`. The same proof block includes `baseline_runs` and
`candidate_runs` child artifact addresses so follow-up reports can consume the
persisted compare record instead of rediscovering temp paths.

Known trace lab plumbing issues are tracked separately and are not papered over by this report path: [#3621](https://github.com/Extra-Chill/homeboy/issues/3621) for Docker preflight detection and [#3631](https://github.com/Extra-Chill/homeboy/issues/3631) for runner daemon restart exec diagnostics.

## Repeated Runs

Use `--runs N` to execute the same trace scenario multiple times and aggregate timing spans. `--repeat N` remains accepted as the same option.

```sh
homeboy trace woocommerce-gateway-stripe ece-product-page-waterfall --runs 5 --aggregate spans
```

Aggregate JSON preserves each run's trace artifact path in `runs[].artifact_path` and keeps raw timing samples under each span:

```json
{
  "command": "trace.aggregate.spans",
  "repeat": 5,
  "run_count": 5,
  "failure_count": 0,
  "runs": [
    { "index": 1, "status": "pass", "artifact_path": "/path/to/run-1/trace-results.json" }
  ],
  "spans": [
    {
      "id": "boot_to_ready",
      "n": 5,
      "min_ms": 94,
      "median_ms": 101,
      "avg_ms": 103.2,
      "stddev_ms": 8.1,
      "max_ms": 118,
      "samples": [
        { "run_index": 1, "duration_ms": 101, "artifact_path": "/path/to/run-1/trace-results.json" }
      ],
      "failures": 0
    }
  ]
}
```

Failed runs remain in `runs` with their status, exit code, and failure message. Spans that were requested but could not be measured count those runs in `failures` while successful runs still contribute their raw samples and min/median/max/average/stddev summary.

## Extension Manifest

```json
{
  "trace": {
    "extension_script": "scripts/trace/trace-runner.sh"
  }
}
```

## Generic Shell Runner

When a component has no trace-capable extension, `homeboy trace` falls back to a built-in generic runner. This is intentionally in core rather than a separate `shell` extension so shell-only or JSON-config components can run trace workloads without installing an extension or adding a fake language marker such as `package.json`. Components that already have a trace extension, including the Node.js extension, continue to use that extension first.

The generic runner discovers workloads in:

- `<component>/traces/*.trace.{mjs,sh,py}`
- `<component>/scripts/trace/*.{mjs,sh,py}`

It also honors `HOMEBOY_TRACE_EXTRA_WORKLOADS` using the platform path separator, matching the existing rig-owned workload handoff pattern. Workloads run from the component directory with the standard trace environment below and are responsible for writing `HOMEBOY_TRACE_RESULTS_FILE`.

Generic workloads are dispatched by extension:

- `.mjs` via `node`
- `.sh` via `sh`
- `.py` via `python3`

## Runner Environment

- `HOMEBOY_TRACE_RESULTS_FILE`
- `HOMEBOY_TRACE_SCENARIO`
- `HOMEBOY_TRACE_LIST_ONLY`
- `HOMEBOY_TRACE_ARTIFACT_DIR`
- `HOMEBOY_TRACE_ATTACHMENTS` when `--attach` is used; JSON array of `{ "kind", "target" }` objects
- `HOMEBOY_TRACE_RIG_ID` when `--rig` is used
- `HOMEBOY_TRACE_COMPONENT_PATH` when Homeboy resolves a path override
- `HOMEBOY_RUN_DIR`

## Probes

Rig-owned trace workloads can declare passive `trace_probes` that Homeboy runs beside the trace runner and merges into the final `timeline`. See [Trace Probes](../architecture/trace-probes.md).

## Results Envelope

```json
{
  "component_id": "studio",
  "scenario_id": "close-window-running-site",
  "status": "fail",
  "summary": "Window reopened after close",
  "timeline": [
    { "t_ms": 0, "source": "desktop", "event": "window.closed", "data": { "id": 1 } }
  ],
  "span_definitions": [
    { "id": "close_to_assertion", "from": "desktop.window.closed", "to": "assertion.checked" }
  ],
  "assertions": [
    { "id": "no-window-reopen", "status": "fail", "message": "Window reopened" }
  ],
  "artifacts": [
    { "label": "main log", "path": "artifacts/main.log" }
  ]
}
```

V1 statuses are `pass`, `fail`, and `error`.

## Attachments

Use repeatable `--attach KIND:TARGET` flags to observe already-running local systems while the selected trace scenario still runs normally. Attachments do not start, stop, restart, or kill the target; they only add before/after observation events to the trace timeline and write an attachment observation artifact in the run directory.

Supported v1 attachment kinds:

- `logfile:<path>` records whether the file exists and its byte length before and after the scenario.
- `fswatch:<path>` records whether a watched file exists, its byte length, and its last-modified timestamp before and after the scenario. It also enables the same passive `file.watch` probe used by rig workloads, so creates, writes, and deletes observed during the scenario are emitted as `file.watch.fs.*` timeline events. V1 is polling-based and does not attribute the writer PID.
- `pid:<n>` records whether a local process exists before and after the scenario.
- `port:<n>` checks whether `127.0.0.1:<n>` accepts TCP connections before and after the scenario.
- `http:<url>` or a direct `http://` / `https://` URL performs a local HTTP GET before and after the scenario and records the response status or connection error.
- `systemd:<unit>` records local `systemctl show` unit state before and after the scenario, including load/active/sub states and the unit main PID when available. It observes an already-running local unit only; it does not start, stop, restart, or SSH to the unit host.

Example:

```sh
homeboy trace wp-coding-agents auth-multi-session-race \
  --attach logfile:/root/.kimaki/kimaki.log \
  --attach fswatch:/home/opencode/.local/share/opencode/auth.json \
  --attach pid:3679661 \
  --attach systemd:kimaki.service \
  --attach http://127.0.0.1:46227/health
```

Core also exports the parsed attachments to the runner through `HOMEBOY_TRACE_ATTACHMENTS` so extension-owned scenarios can correlate their own events with the same observation surfaces. `fswatch` attachments are deduplicated with explicit `file.watch` rig probes for the same path. `systemd:` attachments require local `systemctl`; on non-systemd hosts the timeline records the attachment as unavailable instead of failing the trace. Remote/SSH attach targets remain out of scope.

## Spans

Spans are generic intervals over timeline keys. A timeline key is `source.event`, using the event's `source` and `event` fields.

Runners can emit `span_definitions`, or callers can pass repeatable `--span id:from:to` flags. Homeboy writes computed results back into the command output as `span_results`:

```json
{
  "span_results": [
    {
      "id": "submit_to_cli",
      "from": "ui.create_site.submit_clicked",
      "to": "cli.validating_site_configuration",
      "status": "ok",
      "duration_ms": 1065,
      "from_t_ms": 120,
      "to_t_ms": 1185
    }
  ]
}
```

If an endpoint is missing, Homeboy emits a skipped result with `missing` keys instead of panicking.

When a timeline contains repeated events with the same key, Homeboy resolves the span to the nearest valid `from`/`to` pair where the `to` event occurs at or after the `from` event. This keeps simple `source.event` span definitions stable for common lifecycle events that naturally repeat.

Span endpoints can add a bracketed selector to disambiguate repeated events without inventing extra one-off event names:

```sh
homeboy trace studio create-site \
  --span running:renderer.site_event_received[data.running=true,data.name=site-updated]:renderer.dom_status_running_seen
homeboy trace studio create-site \
  --span second_ready:runner.state[data.phase=ready,occurrence=2]:runner.done
homeboy trace studio create-site \
  --span final_tick:runner.tick[last]:runner.done
```

Supported endpoint selector terms:

- `data.FIELD=value` filters by an event `data` field. Dot paths are supported, for example `data.payload.running=true`.
- `occurrence=N` selects the 1-based Nth event after field filters are applied.
- `last` or `occurrence=last` selects the last event after field filters are applied.

Selector values are parsed as JSON when possible, so `true`, `false`, `42`, and quoted strings use JSON semantics. Unquoted strings such as `site-updated` are treated as string values. The unbracketed `source.event` syntax remains unchanged.

## Temporal Assertions

Runners can declare `temporal_assertions` for timeline-level checks. Homeboy evaluates them after the runner exits, appends the evaluated result to the existing `assertions` list, and marks the trace failed when any evaluated assertion fails. Existing simple runner-emitted assertions still work unchanged.

V1 supports these assertion kinds:

- `count`: count matching timeline keys and enforce optional `min` / `max` bounds.
- `forbidden-event`: fail when a timeline key appears at least once.
- `max-concurrent`: track a start/end event pair and fail when live concurrency exceeds `max`.
- `no-overlap`: fail when two matching events with different `by` data values occur within `window_ms`.
- `ordering`: for each `before` event, require a later `after` event, optionally `within_ms` and with the same `by` data value.
- `latency-bound`: pair each `from` event with the first later `to` event and enforce optional `p50_ms`, `p95_ms`, and `p99_ms` bounds using the same R-7 percentile calculation as `homeboy bench`.
- `required-sequence`: require the listed `source.event` keys to occur as an ordered subsequence in the timeline.

Timeline keys use the same `source.event` format as spans. Failed assertions include a structured `details` object with the observed counts and matching events.

```json
{
  "timeline": [
    { "t_ms": 0, "source": "proc", "event": "spawn" },
    { "t_ms": 5, "source": "proc", "event": "spawn" },
    { "t_ms": 10, "source": "proc", "event": "exit" }
  ],
  "temporal_assertions": [
    {
      "id": "no-invalid-grant",
      "kind": "count",
      "events": ["log.invalid_grant"],
      "max": 0
    },
    {
      "id": "no-window-reopen",
      "kind": "forbidden-event",
      "pattern": "desktop.window.reopened"
    },
    {
      "id": "max-one-proc",
      "kind": "max-concurrent",
      "track": ["proc.spawn", "proc.exit"],
      "max": 1
    },
    {
      "id": "no-auth-write-race",
      "kind": "no-overlap",
      "events": ["fs.write"],
      "by": "pid",
      "window_ms": 100
    },
    {
      "id": "response-before-write",
      "kind": "ordering",
      "before": "http.response",
      "after": "fs.write",
      "within_ms": 100,
      "by": "request_id"
    },
    {
      "id": "request-latency",
      "kind": "latency-bound",
      "from": "request.start",
      "to": "request.end",
      "p95_ms": 250
    },
    {
      "id": "boot-flow",
      "kind": "required-sequence",
      "sequence": ["app.boot", "auth.login", "app.ready"]
    }
  ]
}
```

The evaluated assertion list keeps the normal assertion shape and adds `details` when Homeboy has structured evidence:

```json
{
  "id": "max-one-proc",
  "status": "fail",
  "message": "max concurrency for `proc.spawn` exceeded 1: observed 2",
  "details": {
    "kind": "max-concurrent",
    "track": ["proc.spawn", "proc.exit"],
    "max": 1,
    "max_observed": 2,
    "at_t_ms": 5
  }
}
```

## Phases

Use repeatable `--phase [label:]source.event` flags to provide an ordered milestone chain. Homeboy expands the chain into adjacent span results plus a `phase.total` span from the first milestone to the last milestone:

```sh
homeboy trace studio create-site \
  --phase submit:ui.create_site.submit_clicked \
  --phase cli:studio_server_child.run_cli.before \
  --phase ready:playground.run_cli.ready \
  --report=markdown
```

The example above produces span rows for `phase.submit_to_cli`, `phase.cli_to_ready`, and `phase.total`. Existing `--span` definitions still work and can be mixed with phase milestones.

Phase spans keep the same ordering semantics as normal spans: a phase interval is only `ok` when the later milestone occurs at or after the previous milestone. If both phase milestones exist but the later milestone was first observed before the previous milestone, Homeboy reports the span as skipped with a non-monotonic phase-chain diagnostic instead of treating the out-of-order interval as successful. Markdown reports include that diagnostic in the span status column so asynchronous readiness events are easier to distinguish from missing events.

Rigs and rig-owned trace workloads can declare reusable phase presets. Use `--phase-preset <name>` to expand a named preset from the selected rig/workload into the same adjacent phase spans:

```jsonc
{
  "trace_workloads": {
    "nodejs": [
      {
        "path": "${package.root}/trace/create-site.trace.mjs",
        "trace_default_phase_preset": "create-site",
        "trace_phase_presets": {
          "create-site": [
            "submit:ui.create_site.submit_clicked",
            "cli:studio_server_child.run_cli.before",
            "ready:playground.run_cli.ready"
          ]
        }
      }
    ]
  }
}
```

When `--repeat <N> --aggregate spans` is used with `--rig` and no explicit `--phase`, `--phase-preset`, or `--span` flags, Homeboy applies the workload's `trace_default_phase_preset`. A preset named `default` is also recognized when no explicit default pointer is present.

## Repeat And Aggregate

Use `--repeat <N> --aggregate spans` to run the same trace scenario multiple times and summarize span timings across runs. The aggregate output includes each run's preserved `trace.json` artifact path plus per-span `min_ms`, `median_ms`, `avg_ms`, percentile fields (`p75_ms` with at least 4 samples, `p90_ms` with at least 10, and `p95_ms` with at least 20), `max_ms`, the run index and artifact path for that max sample, and `failures` counts. Markdown aggregate reports also include those percentile columns and an outlier table sorted by max duration so the slowest run artifacts are easy to inspect first.

```sh
homeboy trace studio studio-app-create-site --repeat 5 --aggregate spans
```

Each repeat uses a fresh Homeboy run directory, so completed run data is preserved even when a later repeat fails.

Use `--schedule grouped` or `--schedule interleaved` to record the intended run order in the aggregate manifest. The current single-scenario repeat runner records one `run` group; the planner is shared with future baseline/variant runners so paired experiments can use grouped order (`baseline...variant...`) or interleaved order (`baseline, variant, baseline, variant`).

Use repeatable `--focus-span <span-id>` to add a focused span section while keeping the full span table in the JSON and Markdown report.

## Guardrails

Rig-pinned aggregate traces can run post-trace guardrails after timing artifacts are captured. Guardrails reuse rig `check` probes, so command and HTTP checks are supported with the same fields as pipeline checks. Declare them at the rig level, on a trace workload, or on a named trace variant:

```jsonc
{
  "trace_guardrails": [
    { "label": "app health", "http": "http://127.0.0.1:3000/health", "expect_status": 200 }
  ],
  "trace_workloads": {
    "nodejs": [
      {
        "path": "${package.root}/trace/create-site.trace.mjs",
        "trace_guardrails": [
          { "label": "site still lists", "command": "npm run smoke:list-sites" }
        ]
      }
    ]
  },
  "trace_variants": {
    "fast-install": {
      "overlay": "overlays/fast-install.patch",
      "trace_guardrails": [
        { "label": "install behavior", "command": "npm run smoke:install" }
      ]
    }
  }
}
```

Guardrail failures mark the aggregate or experiment result as failed, but Homeboy still writes the timing artifacts, span summaries, and compare JSON. Compare outputs include before/after guardrail results alongside span deltas so a faster run cannot hide a behavior regression.

## Compare Aggregates

Use `trace compare` to compare two aggregate span JSON outputs. The comparison reports each span's before/after median and average, absolute deltas, and percentage deltas. Spans are sorted by absolute median delta descending so the largest changes are first; spans that only exist in one file are included with unavailable deltas after comparable spans. Markdown reports bold non-zero absolute deltas to make regressions and improvements easier to scan.

```sh
homeboy trace compare before.json after.json
homeboy trace compare before.json after.json --focus-span phase.wp_boot_start_to_wp_boot_ready --report=markdown
```

Focused compare spans are evaluated independently from the full span table. When a focused span's median slowdown exceeds both `--regression-threshold` and `--regression-min-delta-ms`, or its failure count increases, `trace compare` returns a failing exit code and records `focus_status`, `focus_regression_count`, and `focus_failure_count` in JSON output. All compared spans remain present in `spans`.

Trace runners can also emit scalar metrics in the top-level `metrics` object. Repeated trace aggregates summarize each numeric metric with `min`, `median`, `max`, and raw `samples`; compare output includes metric deltas and preserves baseline/candidate sample lists.

Use repeatable `--metric-guardrail` flags to fail compare output when generic scalar policies are violated:

```sh
homeboy trace compare before.json after.json \
  --metric-guardrail request_count:equal \
  --metric-guardrail page_errors.max:lte \
  --metric-guardrail ready_ms:percent:10
```

The syntax is `METRIC[.min|.median|.max]:POLICY[:VALUE]`. The default statistic is `median`. Supported policies are `required`, `equal`, `lte`/`candidate_lte_baseline`, `delta`/`absolute_delta`, and `percent`/`percent_delta`. Threshold policies use absolute deltas so improvements and regressions are both bounded unless the caller chooses `lte` for lower-is-better metrics.

## Scenario Matrices

Use `trace matrix` to run the same trace scenario across a Cartesian product of axis values. Each `--axis` is declared as `name=value1,value2`; repeat the flag for dimensions such as viewport, ECE location, payment method, selected variation state, or feature switches.

```sh
homeboy trace matrix woocommerce-gateway-stripe ece-product-page-waterfall \
  --axis viewport=desktop,mobile \
  --axis ece_locations=product,none \
  --axis methods=card-link,card-only \
  --output-dir .homeboy/experiments/ece-product-matrix
```

For every cell, Homeboy passes axis values through both runner config and environment:

- Config settings include each axis key as a string value plus a `trace_matrix` object containing all cell values.
- Environment includes `HOMEBOY_TRACE_MATRIX_CELL`, `HOMEBOY_TRACE_MATRIX_LABEL`, `HOMEBOY_TRACE_MATRIX_JSON`, and one `HOMEBOY_TRACE_MATRIX_<AXIS>` variable per axis. Non-alphanumeric axis characters become underscores in env-var names.

The output directory keeps a stable artifact set:

- `matrix.json` is the machine-readable matrix summary with cell-level pass/fail, axis values, run artifact paths, and per-cell output paths.
- `summary.md` is a Markdown table for human review.
- `cell-NNN-<axis-label>/trace.json` preserves the trace output for each matrix cell.

The matrix command continues after failed cells so the JSON/table report shows the full matrix. It exits non-zero when any cell fails.

## Compare Variant Experiments

Use `trace compare-variant` to run a baseline aggregate, run the same trace with one or more overlays, compare the aggregate span outputs, and keep the evidence in one directory:

```sh
homeboy trace compare-variant \
  --rig studio \
  --scenario studio-app-create-site \
  --phase-preset wordpress-boot-steps \
  --repeat 5 \
  --overlay overlays/fresh-install-mode.patch \
  --overlay overlays/disable-install-mail.patch \
  --output-dir .homeboy/experiments/fast-install
```

The bundle contains `baseline.json`, `variant.json`, `compare.json`, and `summary.md`. The summary includes component SHAs from rig state when available plus the files touched by each variant overlay.

## Markdown Reports

Use `--report=markdown` to render a skim-friendly report from the same trace run. The report includes status, span table, assertions, artifacts, and timeline events.

## Trace Baselines

Trace spans and evaluated assertions can use the same lifecycle flags as other baseline-aware commands:

- `--baseline` stores the current span durations and evaluated assertion snapshots in `homeboy.json` under `baselines.trace`.
- `--ratchet` updates the stored baseline when spans or assertion metrics improve.
- `--ignore-baseline` skips comparison.
- `--regression-threshold=<PCT>` controls the allowed duration slowdown. Default is `5`.
- `--regression-min-delta-ms=<MS>` controls the minimum absolute slowdown before a regression can fail. Default is `50`.

Both regression thresholds must trip before Homeboy fails the run. For example, `9ms -> 15ms` exceeds the default percentage threshold but stays below the default `50ms` minimum delta, so it does not fail as a trace baseline regression.

Assertion baselines compare evaluated assertion status plus lower-is-better metrics when a temporal assertion exposes one, such as `count.actual`, `forbidden-event.actual`, `max-concurrent.max_observed`, `no-overlap.overlap_count`, `ordering.violation_count`, and `latency-bound` percentile values. This supports assertion-only race checks, for example stderr event counts, without requiring synthetic spans.

Rig-pinned traces store separate baselines under `trace.rig.<rig-id>` so bare and rig-owned traces do not collide.
