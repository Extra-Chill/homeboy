#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required" >&2
    exit 1
fi

RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/datamachine-agent-wp-codebox.XXXXXX")
CONFIG_TMPFILE="$RUNTIME_DIR/config.json"
RESULTS_TMPFILE="$RUNTIME_DIR/results.json"
FAKE_WP_CODEBOX="$RUNTIME_DIR/wp-codebox"
FAKE_ARGS_FILE="$RUNTIME_DIR/wp-codebox-args.txt"
BUNDLE_DIR="$RUNTIME_DIR/bundle"
AGENTS_API_DIR="$RUNTIME_DIR/agents-api"
DATA_MACHINE_DIR="$RUNTIME_DIR/data-machine"
DATA_MACHINE_CODE_DIR="$RUNTIME_DIR/data-machine-code"

cleanup() {
    rm -rf "$RUNTIME_DIR"
}
trap cleanup EXIT

mkdir -p "$BUNDLE_DIR" "$AGENTS_API_DIR" "$DATA_MACHINE_DIR" "$DATA_MACHINE_CODE_DIR"
printf '{"agent":{"slug":"wp-codebox-smoke-agent"}}\n' > "$BUNDLE_DIR/manifest.json"

cat > "$FAKE_WP_CODEBOX" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

args_file="${FAKE_WP_CODEBOX_ARGS_FILE:?}"
printf '%s\n' "$@" > "$args_file"

has_arg() {
    local expected="$1"
    shift
    for arg in "$@"; do
        if [ "$arg" = "$expected" ]; then
            return 0
        fi
    done
    return 1
}

has_arg agent-sandbox-run "$@"
has_arg --json "$@"
has_arg --secret-env "$@"
has_arg HOMEBOY_DATAMACHINE_AGENT_CONFIG "$@"

node - <<'NODE'
const fs = require('node:fs')
const path = require('node:path')

const artifactRoot = path.join(path.dirname(process.env.FAKE_WP_CODEBOX_ARGS_FILE), 'wp-codebox-artifacts', 'runtime-smoke')
const filesRoot = path.join(artifactRoot, 'files')
fs.mkdirSync(filesRoot, { recursive: true })

const changedFiles = {
  schema: 'wp-codebox/changed-files/v1',
  files: [
    {
      path: '/wordpress/wp-content/plugins/example/generated.txt',
      status: 'added',
      mountTarget: '/wordpress/wp-content/plugins/example',
      relativePath: 'generated.txt',
    },
  ],
}
const review = {
  schema: 'wp-codebox/artifact-review/v1',
  artifactId: 'runtime-smoke',
  summary: 'Sandbox produced changes in 1 file.',
  stats: { added: 1, modified: 0, deleted: 0, total: 1 },
  changedFiles: changedFiles.files,
  progress: [{ type: 'complete', label: 'Ready for your review.' }],
  actions: [{ kind: 'approve', label: 'Apply changes', requiresApprovedFiles: true }],
  evidence: {
    patch: 'files/patch.diff',
    patchSha256: 'sha256:example',
    changedFiles: 'files/changed-files.json',
  },
  riskFlags: [],
}

fs.writeFileSync(path.join(artifactRoot, 'manifest.json'), JSON.stringify({ files: [] }, null, 2) + '\n')
fs.writeFileSync(path.join(artifactRoot, 'metadata.json'), JSON.stringify({ artifacts: { review: 'files/review.json', changedFiles: 'files/changed-files.json', patch: 'files/patch.diff' } }, null, 2) + '\n')
fs.writeFileSync(path.join(filesRoot, 'changed-files.json'), JSON.stringify(changedFiles, null, 2) + '\n')
fs.writeFileSync(path.join(filesRoot, 'review.json'), JSON.stringify(review, null, 2) + '\n')
fs.writeFileSync(path.join(filesRoot, 'patch.diff'), 'diff --git a/generated.txt b/generated.txt\n')

const output = {
  metrics: {
    config_present: 1,
    dry_run: 1,
  },
  metadata: {
    provider: 'example-provider',
    model: 'example-model',
    agent_slug: 'wp-codebox-smoke-agent',
    flow_slug: 'wp-codebox-smoke-flow',
    engine_data: {
      ok: true,
    },
  },
}

process.stdout.write(JSON.stringify({
  success: true,
  runtime: {
    backend: 'wordpress-playground',
    status: 'destroyed',
  },
  execution: {
    stdout: JSON.stringify({ output: JSON.stringify(output) }),
  },
  artifacts: {
    directory: artifactRoot,
    manifestPath: path.join(artifactRoot, 'manifest.json'),
    metadataPath: path.join(artifactRoot, 'metadata.json'),
    blueprintAfterPath: path.join(artifactRoot, 'blueprint.after.json'),
    capturedMountsPath: path.join(filesRoot, 'mounted-files.json'),
    changedFilesPath: path.join(filesRoot, 'changed-files.json'),
    patchPath: path.join(filesRoot, 'patch.diff'),
    reviewPath: path.join(filesRoot, 'review.json'),
  },
}) + '\n')
NODE
SH
chmod +x "$FAKE_WP_CODEBOX"

jq -n \
    --arg bundle "$BUNDLE_DIR" \
    --arg agentsApi "$AGENTS_API_DIR" \
    --arg dataMachine "$DATA_MACHINE_DIR" \
    --arg dataMachineCode "$DATA_MACHINE_CODE_DIR" \
    '{
        agent_runtime: "wp-codebox",
        component_id: "wp-codebox-smoke-component",
        component_path: env.PWD,
        bundle_host_path: $bundle,
        agent_slug: "wp-codebox-smoke-agent",
        flow_slug: "wp-codebox-smoke-flow",
        workload_id: "wp-codebox-runner-smoke",
        workload_label: "WP Codebox runner smoke",
        dry_run: true,
        provider: "example-provider",
        model: "example-model",
        prompt: "Smoke the WP Codebox runner adapter.",
        wp_codebox_components: {
            agents_api: $agentsApi,
            data_machine: $dataMachine,
            data_machine_code: $dataMachineCode
        }
    }' > "$CONFIG_TMPFILE"

FAKE_WP_CODEBOX_ARGS_FILE="$FAKE_ARGS_FILE" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_WP_CODEBOX" \
HOMEBOY_DATAMACHINE_AGENT_RESULTS_FILE="$RESULTS_TMPFILE" \
    bash "$SCRIPT_DIR/run-datamachine-agent.sh" "$CONFIG_TMPFILE"

scenario='.scenarios[] | select(.id == "wp-codebox-runner-smoke")'
if ! jq -e "$scenario" "$RESULTS_TMPFILE" >/dev/null; then
    echo "ERROR: wp-codebox-runner-smoke scenario missing" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi

for metric in config_present_mean dry_run_mean; do
    value=$(jq -r "$scenario | .metrics.${metric} // \"missing\"" "$RESULTS_TMPFILE")
    if [ "$value" != "1" ]; then
        echo "ERROR: ${metric} expected 1, got ${value}" >&2
        cat "$RESULTS_TMPFILE" >&2
        exit 1
    fi
done

wp_codebox_success=$(jq -r "$scenario | .metadata.wp_codebox.success // false" "$RESULTS_TMPFILE")
artifact_dir=$(jq -r "$scenario | .metadata.wp_codebox.artifacts.directory // \"\"" "$RESULTS_TMPFILE")
review_schema=$(jq -r "$scenario | .metadata.wp_codebox.review_payload.schema // \"missing\"" "$RESULTS_TMPFILE")
changed_files_schema=$(jq -r "$scenario | .metadata.wp_codebox.changed_files.schema // \"missing\"" "$RESULTS_TMPFILE")
review_artifact=$(jq -r "$scenario | .artifacts.wp_codebox_review.kind // \"missing\"" "$RESULTS_TMPFILE")
patch_artifact=$(jq -r "$scenario | .artifacts.wp_codebox_patch.kind // \"missing\"" "$RESULTS_TMPFILE")
if [ "$wp_codebox_success" != "true" ] || [ -z "$artifact_dir" ] || [ "$review_schema" != "wp-codebox/artifact-review/v1" ] || [ "$changed_files_schema" != "wp-codebox/changed-files/v1" ] || [ "$review_artifact" != "review" ] || [ "$patch_artifact" != "patch" ]; then
    echo "ERROR: wp_codebox metadata missing (success=$wp_codebox_success artifact_dir=$artifact_dir)" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi

require_arg_pair() {
    local name="$1"
    local value="$2"
    local previous=""
    local line=""
    while IFS= read -r line; do
        if [ "$previous" = "$name" ] && [ "$line" = "$value" ]; then
            return 0
        fi
        previous="$line"
    done < "$FAKE_ARGS_FILE"

    echo "ERROR: expected wp-codebox arg pair $name $value" >&2
    cat "$FAKE_ARGS_FILE" >&2
    exit 1
}

require_arg_pair --agents-api "$AGENTS_API_DIR"
require_arg_pair --data-machine "$DATA_MACHINE_DIR"
require_arg_pair --data-machine-code "$DATA_MACHINE_CODE_DIR"
require_arg_pair --mount "$EXTENSION_PATH:/homeboy-extension:readonly"
require_arg_pair --mount "$BUNDLE_DIR:/wordpress/wp-content/plugins/bundle:readonly"
require_arg_pair --secret-env HOMEBOY_DATAMACHINE_AGENT_CONFIG

echo "✓ WP Codebox Data Machine agent runner smoke test PASSED"
