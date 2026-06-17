# Node.js Trace Helpers

Node.js trace workloads can import reusable helpers from the directory exposed as
`HOMEBOY_TRACE_HELPER_DIR` by `trace-runner.sh`.

```js
import { pathToFileURL } from 'node:url';

const helperDir = process.env.HOMEBOY_TRACE_HELPER_DIR;
const { createTraceWorkload } = await import(pathToFileURL(`${helperDir}/timeline.mjs`).href);
const { createHttpStatusHistory, pollHttp, pollJsonFile, pollProcess, parseLogLines } = await import(pathToFileURL(`${helperDir}/probes.mjs`).href);

const trace = createTraceWorkload();

await pollHttp('http://127.0.0.1:3000/', {
	readyStatus: [200, 399],
	timeoutMs: 30000,
	intervalMs: 250,
	requestTimeoutMs: 1000,
	onEvent: trace.event,
});

trace.assertion('app-ready', 'pass', 'App became ready.');
await trace.pass({}, { summary: 'Trace completed' });
```

## Helper Modules

- `timeline.mjs` — `createTraceWorkload()` exposes generic `event`, `artifact`, `assertion`, `check`, `pass`, and `fail` helpers for workloads. `createTraceRecorder()` remains available when a workload needs direct recorder access. Both paths write timeline JSONL, assertions, artifacts, and the final trace envelope through the same normalized shapes.
- `process.mjs` — process launch, cleanup, exit waiting, and process-tree capture helpers.
- `desktop.mjs` — best-effort desktop observation helpers for local trace evidence.
- `probes.mjs` — app-agnostic polling and bridge helpers for common trace observations.

## Probe Helpers

- `pollHttp(url, options)` emits `http.first_response`, transition-only `http.status`, compact `http.status_summary`, `http.ready`, and `http.timeout` events. The ready/timeout result includes `status_history`, `repeated_status_count`, and `last_non_ready_status`.
- `createHttpStatusHistory()` records status transitions and repeated-status counts for workloads that need to probe HTTP-style readiness manually.
- `pollJsonFile(filePath, options)` tolerates missing files and mid-write JSON parse failures, then emits configured events once their predicates match.
- `pollProcess(pattern, options)` matches process command lines by string or regex and emits `process.seen`, `process.gone`, or `process.timeout`.
- `parseLogLines(text, patterns, onEvent)` turns text lines into timeline events using regex patterns.
- `installConsoleBridge(page, options)` attaches to a Playwright/Puppeteer-style `page.on('console')` interface and records prefixed console messages.
- `withObservationWindow(promise, timeoutMs, options)` bounds helper observations so trace workloads do not run forever.

Keep workload-specific semantics in the workload. These helpers should stay generic: no Studio, WordPress, Electron, or browser-specific assumptions beyond the optional page-like console bridge interface.
