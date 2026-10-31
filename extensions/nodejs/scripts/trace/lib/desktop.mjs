import { execFile } from 'node:child_process';
import { platform } from 'node:os';
import { promisify } from 'node:util';
import { artifactPath } from './artifacts.mjs';

const execFileAsync = promisify(execFile);

export async function observeVisibleWindows(options = {}) {
    if (platform() !== 'darwin') {
        return skipped(`Visible window observation is not implemented on ${platform()}.`);
    }

    const script = `
set output to ""
tell application "System Events"
  repeat with proc in (application processes whose visible is true)
    set procName to name of proc
    repeat with win in windows of proc
      try
        set winTitle to name of win
        set winPosition to position of win
        set winSize to size of win
        set output to output & procName & tab & winTitle & tab & (item 1 of winPosition) & "," & (item 2 of winPosition) & tab & (item 1 of winSize) & "," & (item 2 of winSize) & linefeed
      end try
    end repeat
  end repeat
end tell
return output
`;

    try {
        const { stdout } = await execFileAsync('osascript', ['-e', script], { timeout: options.timeoutMs || 5000 });
        const windows = stdout.split(/\r?\n/).filter(Boolean).map(parseWindowLine);
        return { status: 'captured', windows };
    } catch (err) {
        return { status: 'unknown', reason: `Unable to observe visible windows: ${err.message}`, windows: [] };
    }
}

export async function captureScreenshot(name = 'screenshot.png', options = {}) {
    if (platform() !== 'darwin') {
        return { status: 'skipped', reason: `Screenshot capture is not implemented on ${platform()}.` };
    }

    const path = artifactPath(name);
    const args = options.interactive ? [path] : ['-x', path];

    try {
        await execFileAsync('screencapture', args, { timeout: options.timeoutMs || 10000 });
        if (options.recorder) {
            options.recorder.addArtifact(options.label || 'screenshot', path, 'image/png');
            await options.recorder.recordEvent('desktop', 'screenshot.captured', { path: name });
        }
        return { status: 'captured', path };
    } catch (err) {
        return { status: 'unknown', reason: `Unable to capture screenshot: ${err.message}` };
    }
}

function skipped(reason) {
    return { status: 'skipped', reason, windows: [] };
}

function parseWindowLine(line) {
    const [app, title, position, size] = line.split('\t');
    const [x, y] = (position || '').split(',').map((value) => Number(value));
    const [width, height] = (size || '').split(',').map((value) => Number(value));
    return {
        app,
        title,
        bounds: { x, y, width, height },
    };
}
