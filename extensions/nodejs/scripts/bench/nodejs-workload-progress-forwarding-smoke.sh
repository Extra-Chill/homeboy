#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-workload-progress.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

RELEASE_FILE="$TMP_DIR/release"
CAPTURE_FILE="$TMP_DIR/capture"
FORWARDED_FILE="$TMP_DIR/forwarded"
DRIVER_FILE="$TMP_DIR/driver.mjs"

cat > "$DRIVER_FILE" <<'JS'
import { writeFile } from 'node:fs/promises';

const { runCommand } = await import(process.env.WORKLOAD_UTILS_UNDER_TEST);
const prefix = 'HOMEBOY_RUNNER_PROGRESS ';
const validOne = `${prefix}{"schema":"homeboy/runner-progress/v1","phase":"import","completed":1,"total":2}`;
const validTwo = `${prefix}{"schema":"homeboy/runner-progress/v1","phase":"render","current_item":"home"}`;
const malformed = `${prefix}{not-json}`;
const invalid = `${prefix}{"schema":"homeboy/runner-progress/v1","phase":"done","status":"succeeded"}`;
const empty = `${prefix}{"schema":"homeboy/runner-progress/v1","metadata":null}`;
const partial = `${prefix}{"schema":"homeboy/runner-progress/v1","phase":"partial"}`;
const expected = `ordinary before\n${validOne}\n${malformed}\n${invalid}\n${empty}\n${validTwo}\nordinary after\n${partial}`;
const child = `
  import { existsSync } from 'node:fs';
  const chunks = ${JSON.stringify([
    'ordinary before\nHOMEBOY_RUNNER_PRO',
    'GRESS {"schema":"homeboy/runner-progress/v1","phase":"import","completed":1,"total":2}\nHOMEBOY_RUNNER_PROGRESS {not-json}\nHOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"done","status":"succeeded"}\nHOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","metadata":null}\nHOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"render","current_item":"home"}\nordinary after\nHOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"partial"}',
  ])};
  for (const chunk of chunks) process.stdout.write(chunk);
  const wait = setInterval(() => {
    if (existsSync(process.env.RELEASE_FILE)) clearInterval(wait);
  }, 10);
`;

const result = await runCommand(process.execPath, ['--input-type=module', '--eval', child], {
  env: { RELEASE_FILE: process.env.RELEASE_FILE },
  redact: false,
});
await writeFile(process.env.CAPTURE_FILE, result.stdout);
if (result.stdout !== expected) {
  throw new Error(`captured stdout changed: ${JSON.stringify(result.stdout)}`);
}
JS

WORKLOAD_UTILS_UNDER_TEST="$SCRIPT_DIR/lib/workload-utils.mjs" \
RELEASE_FILE="$RELEASE_FILE" \
CAPTURE_FILE="$CAPTURE_FILE" \
    node "$DRIVER_FILE" >"$FORWARDED_FILE" &
DRIVER_PID=$!

for _ in $(seq 1 100); do
    if [ -s "$FORWARDED_FILE" ]; then
        break
    fi
    sleep 0.01
done

EXPECTED_FORWARDED=$'HOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"import","completed":1,"total":2}\nHOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"render","current_item":"home"}'
if [ "$(<"$FORWARDED_FILE")" != "$EXPECTED_FORWARDED" ]; then
    echo "Expected only complete canonical progress before child exit:" >&2
    cat "$FORWARDED_FILE" >&2
    touch "$RELEASE_FILE"
    wait "$DRIVER_PID" || true
    exit 1
fi

touch "$RELEASE_FILE"
wait "$DRIVER_PID"

EXPECTED_CAPTURE=$'ordinary before\nHOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"import","completed":1,"total":2}\nHOMEBOY_RUNNER_PROGRESS {not-json}\nHOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"done","status":"succeeded"}\nHOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","metadata":null}\nHOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"render","current_item":"home"}\nordinary after\nHOMEBOY_RUNNER_PROGRESS {"schema":"homeboy/runner-progress/v1","phase":"partial"}'
if [ "$(<"$CAPTURE_FILE")" != "$EXPECTED_CAPTURE" ]; then
    echo "Captured stdout was not preserved exactly:" >&2
    cat "$CAPTURE_FILE" >&2
    exit 1
fi

echo "Node.js workload progress forwarding smoke passed."
