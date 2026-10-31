# Node.js Trace Helpers

The Node.js trace runner exposes reusable helpers for black-box desktop and
window lifecycle scenarios. Scenarios can import them from
`$HOMEBOY_TRACE_HELPER_DIR`.

```js
const helperDir = process.env.HOMEBOY_TRACE_HELPER_DIR;
const { createTraceWorkload } = await import(`${helperDir}/timeline.mjs`);
const { launchProcess, captureProcessTree, waitForExit } = await import(`${helperDir}/process.mjs`);
const { pollHttp } = await import(`${helperDir}/probes.mjs`);

const trace = createTraceWorkload();
```

## Stable Helpers

- `timeline.mjs` records timestamped events, assertions, artifacts, and writes
  the final Homeboy trace JSON envelope. `createTraceWorkload()` is the default
  workload-facing surface: use `trace.event(source, event, data)`,
  `trace.artifact(label, path, kind)`, `trace.assertion(id, status, message,
  data)`, `trace.check(id, ok, message, data)`, `trace.pass(metrics, options)`,
  and `trace.fail(error, metrics, options)` so artifact registration and result
  envelope writing stay centralized. `createTraceRecorder()` remains available
  for direct recorder access. `TraceRecorder#recordCheck()` maps a boolean check
  to a pass/fail assertion, and `TraceRecorder#writeArtifact()` writes an
  artifact under `HOMEBOY_TRACE_ARTIFACT_DIR` while registering it in the result
  envelope. Timeline, assertion, artifact, and envelope rows use the shared
  product-neutral shapes documented in `../../docs/browser-result-shapes.md`.
- `process.mjs` launches black-box commands, captures a best-effort process
  tree artifact on macOS/Linux, waits for exits, and cleans up spawned process
  groups on process exit/signals.
- `artifacts.mjs` resolves artifact paths under `HOMEBOY_TRACE_ARTIFACT_DIR` and
  returns paths relative to that directory for trace envelopes.
- `process.sh` provides shell equivalents for simple scenarios:
  `trace_launch`, `trace_process_tree`, `trace_tail_log`, and automatic cleanup.
- `probes.mjs` provides generic readiness and observation helpers. `pollHttp()`
  records first response, status transitions, compact repeated-status history,
  ready, and timeout evidence without emitting one event per poll. Use
  `createHttpStatusHistory()` when a workload has to drive its own HTTP-style
  probing loop. `installConsoleBridge()` and `captureTraceEventText()` parse
  prefixed JSON bridge events. Payloads shaped as `{ source, event, data }` are
  dispatched as structured timeline events; other payloads fall back to the
  configured bridge source/event.

## Platform-Specific Helpers

- `desktop.mjs` is macOS-first. `observeVisibleWindows()` uses System Events and
  returns `{ status: 'skipped' }` on unsupported platforms. Permission or
  automation failures return `{ status: 'unknown' }`, not a thrown exception.
- `captureScreenshot()` uses macOS `screencapture` and similarly degrades to
  `skipped` or `unknown` evidence when unavailable.

Helpers are intentionally generic and contain no target-app-specific behavior.

## Fixture

`scripts/trace/fixtures/helper.trace.mjs` is a runnable sample scenario. It
launches a dummy Node process, records timeline events and assertions, captures
a process tree artifact, performs best-effort visible-window observation, and
writes a valid Homeboy trace envelope.
