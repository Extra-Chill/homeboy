import { pathToFileURL } from 'node:url';
import { EventEmitter } from 'node:events';
import { createServer } from 'node:http';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const helperDir = process.env.HOMEBOY_TRACE_HELPER_DIR;
const { createTraceRecorder } = await import(pathToFileURL(`${helperDir}/timeline.mjs`).href);
const { launchProcess, waitForExit, captureProcessTree } = await import(pathToFileURL(`${helperDir}/process.mjs`).href);
const { observeVisibleWindows } = await import(pathToFileURL(`${helperDir}/desktop.mjs`).href);
const {
    createHttpStatusHistory,
    installConsoleBridge,
    parseLogLines,
    pollHttp,
    pollJsonFile,
    pollProcess,
    withObservationWindow,
} = await import(pathToFileURL(`${helperDir}/probes.mjs`).href);

const recorder = createTraceRecorder();
await recorder.recordEvent('scenario', 'helper.start', { helperDir: Boolean(helperDir) });
const onEvent = recorder.recordEvent.bind(recorder);

const child = launchProcess('node', {
    args: ['-e', 'setTimeout(() => process.exit(0), 50)'],
    shell: false,
    recorder,
});

await captureProcessTree(child.pid, 'process-tree.txt', { recorder });
const exit = await waitForExit(child);
await recorder.recordEvent('process', 'process.exit', exit);
recorder.recordAssertion('dummy-process-exited', exit.code === 0 ? 'pass' : 'fail', `dummy process exited with ${exit.code}`);

const jsonPath = join(process.env.HOMEBOY_TRACE_ARTIFACT_DIR, 'state.json');
const jsonPoll = pollJsonFile(jsonPath, {
    select: (json) => json.site,
    intervalMs: 20,
    timeoutMs: 1000,
    events: [
        { name: 'json.site_seen', when: (value) => Boolean(value?.name), data: (value) => value },
        { name: 'json.port_known', when: (value) => Number(value?.port) > 0, data: (value) => ({ port: value.port }), terminal: true },
    ],
    onEvent,
});
await writeFile(jsonPath, '{');
setTimeout(() => {
    void writeFile(jsonPath, JSON.stringify({ site: { name: 'demo', port: 9876 } }));
}, 30);
const jsonResult = await jsonPoll;
recorder.recordAssertion('json-poll-port-known', jsonResult.status === 'matched' && jsonResult.event === 'json.port_known' ? 'pass' : 'fail', `json poll returned ${jsonResult.status}`);

let requestCount = 0;
const server = createServer((_, res) => {
    requestCount += 1;
    res.statusCode = requestCount <= 2 ? 502 : requestCount === 3 ? 302 : 200;
    if (res.statusCode === 302) {
        res.setHeader('Location', '/wp-admin/install.php');
    }
    res.end('ok');
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
try {
    const { port } = server.address();
    const httpResult = await pollHttp(`http://127.0.0.1:${port}/`, {
        readyStatus: 200,
        intervalMs: 20,
        requestTimeoutMs: 500,
        timeoutMs: 1000,
        onEvent,
    });
    const expectedHistory = JSON.stringify([{ status: 502, count: 2 }, { status: 302, count: 1, location: '/wp-admin/install.php' }, { status: 200, count: 1 }]);
    const actualHistoryWithLocation = JSON.stringify(httpResult.status_history.map(({ status, count, location }) => ({ status, count, ...(location ? { location } : {}) })));
    recorder.recordAssertion('http-poll-ready', httpResult.status === 'ready' && httpResult.http_status === 200 ? 'pass' : 'fail', `http poll returned ${httpResult.status}`);
    recorder.recordAssertion('http-status-history', actualHistoryWithLocation === expectedHistory && httpResult.last_non_ready_status === 302 ? 'pass' : 'fail', `http status history was ${actualHistoryWithLocation}`);
} finally {
    await new Promise((resolve) => server.close(resolve));
}

const manualHistory = createHttpStatusHistory();
manualHistory.record(502);
manualHistory.record(502);
manualHistory.record(200);
const manualSummary = manualHistory.summary({ lastNonReadyStatus: 502 });
recorder.recordAssertion('http-status-history-helper', manualSummary.repeated_status_count === 1 && manualSummary.last_non_ready_status === 502 ? 'pass' : 'fail', 'manual HTTP status history summarized repeated statuses');

const sleeper = launchProcess('node', {
    args: ['-e', 'setTimeout(() => process.exit(0), 300)'],
    shell: false,
});
const processResult = await pollProcess(/setTimeout\(\(\) => process\.exit\(0\), 300\)/, {
    intervalMs: 20,
    timeoutMs: 1000,
    onEvent,
});
recorder.recordAssertion('process-poll-seen', processResult.status === 'seen' ? 'pass' : 'fail', `process poll returned ${processResult.status}`);
await waitForExit(sleeper);

await parseLogLines('ready on 127.0.0.1:1234\nignored', [
    { event: 'log.port_known', pattern: /ready on (?<host>[^:]+):(?<port>\d+)/, data: (match) => ({ host: match.groups.host, port: Number(match.groups.port) }) },
], onEvent);

const observed = await withObservationWindow(new Promise((resolve) => setTimeout(() => resolve('done'), 20)), 100, { onEvent });
recorder.recordAssertion('observation-window-resolved', observed === 'done' ? 'pass' : 'fail', 'observation window resolved before timeout');

const page = new EventEmitter();
installConsoleBridge(page, { prefix: 'trace:', onEvent });
page.emit('console', { text: () => 'trace:{"event":"bridge-ok"}' });

const windows = await observeVisibleWindows();
recorder.recordAssertion(
    'window-observation-best-effort',
    ['captured', 'skipped', 'unknown'].includes(windows.status) ? 'pass' : 'fail',
    `window observation returned ${windows.status}`
);

await recorder.writeTraceResults({ summary: 'helper scenario passed' });
