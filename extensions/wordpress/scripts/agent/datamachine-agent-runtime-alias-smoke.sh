#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNTIME_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-datamachine-agent-runtime-alias.XXXXXX")"

cleanup() {
    rm -rf "$RUNTIME_DIR"
}
trap cleanup EXIT

CONFIG_PATH="$RUNTIME_DIR/config.json"
RESULTS_PATH="$RUNTIME_DIR/results.json"
CAPTURE_PATH="$RUNTIME_DIR/fake-codebox-capture.json"
FAKE_BIN="$RUNTIME_DIR/fake-codebox.js"
ARTIFACTS_DIR="$RUNTIME_DIR/runtime-artifacts"
MOUNT_SOURCE="$RUNTIME_DIR/runtime-mount"

mkdir -p \
    "$RUNTIME_DIR/component" \
    "$RUNTIME_DIR/wp-codebox" \
    "$RUNTIME_DIR/agents-api" \
    "$RUNTIME_DIR/data-machine" \
    "$RUNTIME_DIR/data-machine-code" \
    "$MOUNT_SOURCE"

cat >"$FAKE_BIN" <<'JS'
#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');

const args = process.argv.slice(2);
const capturePath = process.env.FAKE_CODEBOX_CAPTURE_PATH;

function argValue(name) {
  const index = args.indexOf(name);
  return index === -1 ? '' : args[index + 1] || '';
}

function appendCapture(entry) {
  const current = fs.existsSync(capturePath) ? JSON.parse(fs.readFileSync(capturePath, 'utf8')) : [];
  current.push(entry);
  fs.writeFileSync(capturePath, JSON.stringify(current, null, 2));
}

if (args[0] === 'recipe-run') {
  const recipePath = argValue('--recipe');
  const artifactsDir = argValue('--artifacts');
  const recipe = JSON.parse(fs.readFileSync(recipePath, 'utf8'));
  fs.mkdirSync(path.join(artifactsDir, 'files'), { recursive: true });
  fs.writeFileSync(path.join(artifactsDir, 'manifest.json'), '{}\n');
  fs.writeFileSync(path.join(artifactsDir, 'files', 'transcript.json'), '{}\n');
  fs.writeFileSync(path.join(artifactsDir, 'files', 'agent-result.json'), '{}\n');
  fs.writeFileSync(path.join(artifactsDir, 'files', 'completion-outcome.json'), '{}\n');
  appendCapture({ command: 'recipe-run', args, recipe });
  process.stdout.write(JSON.stringify({
    success: true,
    runtime: { id: 'wp-codebox' },
    artifacts: {
      directory: artifactsDir,
      manifestPath: path.join(artifactsDir, 'manifest.json')
    }
  }) + '\n');
  process.exit(0);
}

if (args[0] === 'artifacts' && args[1] === 'verify') {
  appendCapture({ command: 'artifacts verify', args });
  process.stdout.write(JSON.stringify({ success: true }) + '\n');
  process.exit(0);
}

throw new Error(`unexpected fake codebox invocation: ${args.join(' ')}`);
JS
chmod +x "$FAKE_BIN"

jq -n \
    --arg componentPath "$RUNTIME_DIR/component" \
    --arg runtimeBin "$FAKE_BIN" \
    --arg artifactsDir "$ARTIFACTS_DIR" \
    --arg wpCodebox "$RUNTIME_DIR/wp-codebox" \
    --arg agentsApi "$RUNTIME_DIR/agents-api" \
    --arg dataMachine "$RUNTIME_DIR/data-machine" \
    --arg dataMachineCode "$RUNTIME_DIR/data-machine-code" \
    --arg mountSource "$MOUNT_SOURCE" \
    '{
        workload_id: "runtime-alias-smoke",
        workload_label: "Runtime alias smoke",
        component_path: $componentPath,
        runtime_bin: $runtimeBin,
        runtime_artifacts_dir: $artifactsDir,
        runtime_components: {
            runtime: $wpCodebox,
            agents_api: $agentsApi,
            data_machine: $dataMachine,
            data_machine_code: $dataMachineCode
        },
        runtime_mounts: [
            {source: $mountSource, target: "/wordpress/wp-content/uploads/runtime-mount", mode: "readonly"}
        ]
    }' >"$CONFIG_PATH"

HOMEBOY_DATAMACHINE_AGENT_RESULTS_FILE="$RESULTS_PATH" \
FAKE_CODEBOX_CAPTURE_PATH="$CAPTURE_PATH" \
    bash "$SCRIPT_DIR/run-datamachine-agent.sh" "$CONFIG_PATH" >/dev/null

jq -e '
    .[0].command == "recipe-run"
    and .[0].recipe.schema == "wp-codebox/workspace-recipe/v1"
    and ([.[0].recipe.inputs.extra_plugins[] | select(.slug == "wp-codebox").source] | first | endswith("/wp-codebox"))
    and ([.[0].recipe.inputs.extra_plugins[] | select(.slug == "agents-api").source] | first | endswith("/agents-api"))
    and ([.[0].recipe.inputs.mounts[] | select(.target == "/wordpress/wp-content/uploads/runtime-mount").mode] | first) == "readonly"
' "$CAPTURE_PATH" >/dev/null

jq -e '.scenarios[] | select(.id == "runtime-alias-smoke") | .metrics.config_present_mean == 1' "$RESULTS_PATH" >/dev/null

printf '%s\n' 'Data Machine agent runtime alias smoke passed'
