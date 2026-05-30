import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, writeFile } from 'node:fs/promises';
import { platform } from 'node:os';
import { dirname } from 'node:path';
import { promisify } from 'node:util';
import { artifactPath } from './artifacts.mjs';

const execFileAsync = promisify(execFile);
const spawned = new Set();
let cleanupInstalled = false;

export function launchProcess(command, options = {}) {
    const child = spawn(command, options.args || [], {
        cwd: options.cwd || process.env.HOMEBOY_COMPONENT_PATH || process.cwd(),
        env: { ...process.env, ...(options.env || {}) },
        shell: options.shell ?? true,
        detached: options.detached ?? true,
        stdio: options.stdio || 'ignore',
    });

    spawned.add(child);
    installCleanupHandlers();
    child.once('exit', () => spawned.delete(child));

    if (options.recorder) {
        void options.recorder.recordEvent('process', 'process.launch', {
            command,
            pid: child.pid || null,
        });
    }

    return child;
}

export async function waitForExit(child) {
    if (child.exitCode !== null || child.signalCode !== null) {
        return { code: child.exitCode, signal: child.signalCode };
    }

    const [code, signal] = await once(child, 'exit');
    return { code, signal };
}

export async function captureProcessTree(pid, outputName = 'process-tree.txt', options = {}) {
    const outputPath = artifactPath(outputName);
    const result = await processTree(pid);

    await mkdir(dirname(outputPath), { recursive: true });
    await writeFile(outputPath, result.text);

    if (options.recorder) {
        options.recorder.addArtifact(options.label || 'process tree', outputPath, 'text');
        await options.recorder.recordEvent('process', 'process.tree.captured', {
            pid,
            status: result.status,
        });
    }

    return { ...result, path: outputPath };
}

export async function processTree(pid) {
    if (!pid || !Number.isFinite(Number(pid))) {
        return { status: 'unknown', text: 'No pid provided.\n' };
    }

    if (!['darwin', 'linux'].includes(platform())) {
        return { status: 'skipped', text: `Process tree capture is not implemented on ${platform()}.\n` };
    }

    try {
        const { stdout } = await execFileAsync('ps', ['-axo', 'pid=,ppid=,stat=,comm=,args='], { maxBuffer: 1024 * 1024 });
        const lines = stdout.split(/\r?\n/).filter(Boolean);
        const tree = selectProcessTree(lines, Number(pid));
        return {
            status: tree.length > 0 ? 'captured' : 'unknown',
            text: tree.length > 0 ? `${tree.join('\n')}\n` : `No process tree found for pid ${pid}.\n`,
        };
    } catch (err) {
        return { status: 'unknown', text: `Unable to capture process tree for pid ${pid}: ${err.message}\n` };
    }
}

export async function cleanupSpawnedProcesses(signal = 'SIGTERM') {
    const children = [...spawned];
    spawned.clear();

    for (const child of children) {
        if (!child.pid || child.exitCode !== null) continue;
        try {
            process.kill(-child.pid, signal);
        } catch {
            try {
                child.kill(signal);
            } catch {
                // Best-effort cleanup only.
            }
        }
    }
}

function installCleanupHandlers() {
    if (cleanupInstalled) return;
    cleanupInstalled = true;

    for (const signal of ['SIGINT', 'SIGTERM']) {
        process.once(signal, async () => {
            await cleanupSpawnedProcesses();
            process.kill(process.pid, signal);
        });
    }

    process.once('exit', () => {
        for (const child of spawned) {
            if (!child.pid || child.exitCode !== null) continue;
            try {
                process.kill(-child.pid, 'SIGTERM');
            } catch {
                try {
                    child.kill('SIGTERM');
                } catch {
                    // Best-effort cleanup only.
                }
            }
        }
    });
}

function selectProcessTree(lines, rootPid) {
    const rows = lines.map(parsePsLine).filter(Boolean);
    const byParent = new Map();
    for (const row of rows) {
        if (!byParent.has(row.ppid)) byParent.set(row.ppid, []);
        byParent.get(row.ppid).push(row);
    }

    const root = rows.find((row) => row.pid === rootPid);
    if (!root) return [];

    const output = [];
    const walk = (row, depth) => {
        output.push(`${'  '.repeat(depth)}${row.pid} ${row.stat} ${row.command}`);
        for (const child of byParent.get(row.pid) || []) walk(child, depth + 1);
    };
    walk(root, 0);
    return output;
}

function parsePsLine(line) {
    const match = line.trim().match(/^(\d+)\s+(\d+)\s+(\S+)\s+\S+\s+(.+)$/);
    if (!match) return null;
    return {
        pid: Number(match[1]),
        ppid: Number(match[2]),
        stat: match[3],
        command: match[4],
    };
}
