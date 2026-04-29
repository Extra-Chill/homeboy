import { pathToFileURL } from 'node:url';

const helperDir = process.env.HOMEBOY_TRACE_HELPER_DIR;
const { createTraceRecorder } = await import(pathToFileURL(`${helperDir}/timeline.mjs`).href);
const { launchProcess, waitForExit, captureProcessTree } = await import(pathToFileURL(`${helperDir}/process.mjs`).href);
const { observeVisibleWindows } = await import(pathToFileURL(`${helperDir}/desktop.mjs`).href);

const recorder = createTraceRecorder();
await recorder.recordEvent('scenario', 'helper.start', { helperDir: Boolean(helperDir) });

const child = launchProcess('node', {
    args: ['-e', 'setTimeout(() => process.exit(0), 50)'],
    shell: false,
    recorder,
});

await captureProcessTree(child.pid, 'process-tree.txt', { recorder });
const exit = await waitForExit(child);
await recorder.recordEvent('process', 'process.exit', exit);
recorder.recordAssertion('dummy-process-exited', exit.code === 0 ? 'pass' : 'fail', `dummy process exited with ${exit.code}`);

const windows = await observeVisibleWindows();
recorder.recordAssertion(
    'window-observation-best-effort',
    ['captured', 'skipped', 'unknown'].includes(windows.status) ? 'pass' : 'fail',
    `window observation returned ${windows.status}`
);

await recorder.writeTraceResults({ summary: 'helper scenario passed' });
