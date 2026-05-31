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
REPLAY_BUNDLE_DIR="$RUNTIME_DIR/replay-bundles"
FAKE_WP_CODEBOX="$RUNTIME_DIR/wp-codebox.js"
FAKE_ARGS_FILE="$RUNTIME_DIR/wp-codebox-args.txt"
BUNDLE_DIR="$RUNTIME_DIR/bundle"
AGENTS_API_DIR="$RUNTIME_DIR/agents-api"
DATA_MACHINE_DIR="$RUNTIME_DIR/data-machine"
DATA_MACHINE_CODE_DIR="$RUNTIME_DIR/data-machine-code"
FILE_MOUNT_PATH="$RUNTIME_DIR/fixture-config.json"
DIR_MOUNT_PATH="$RUNTIME_DIR/fixture-directory-mount"

cleanup() {
    rm -rf "$RUNTIME_DIR"
}
trap cleanup EXIT

mkdir -p "$BUNDLE_DIR" "$AGENTS_API_DIR" "$DATA_MACHINE_DIR" "$DATA_MACHINE_CODE_DIR" "$DIR_MOUNT_PATH"
printf '{"agent":{"slug":"wp-codebox-smoke-agent"}}\n' > "$BUNDLE_DIR/manifest.json"
printf '{"ok":true}\n' > "$FILE_MOUNT_PATH"

cat > "$FAKE_WP_CODEBOX" <<'NODE'
#!/usr/bin/env node
const fs = require('node:fs')
const path = require('node:path')

const args = process.argv.slice(2)
const argsFile = process.env.FAKE_WP_CODEBOX_ARGS_FILE
if (!argsFile) {
  throw new Error('FAKE_WP_CODEBOX_ARGS_FILE is required')
}
fs.appendFileSync(argsFile, `${args.join('\n')}\n---\n`)

if (args[0] === 'artifacts' && args[1] === 'verify') {
  if (!args.includes('--artifacts') || !args.includes('--json')) {
    throw new Error('artifact verifier missing --artifacts or --json')
  }
  process.stdout.write(JSON.stringify({
    success: true,
    schema: 'wp-codebox/artifact-verification/v1',
    checks: [{ id: 'manifest', status: 'passed' }],
  }) + '\n')
  process.exit(0)
}

if (args[0] === 'workspace-policy' && args[1] === 'check') {
  if (!args.includes('--input') || !args.includes('--json')) {
    throw new Error('workspace policy check missing --input or --json')
  }
  process.stdout.write(JSON.stringify({
    success: true,
    schema: 'wp-codebox/workspace-policy-check/v1',
    checks: [{ id: 'allowed-files', status: 'passed' }],
  }) + '\n')
  process.exit(0)
}

for (const expected of ['recipe-run', '--recipe', '--json']) {
  if (!args.includes(expected)) {
    throw new Error(`missing expected wp-codebox arg: ${expected}`)
  }
}

const valueAfter = (name) => {
  const index = args.indexOf(name)
  if (index === -1 || index + 1 >= args.length) {
    throw new Error(`missing expected wp-codebox arg value: ${name}`)
  }
  return args[index + 1]
}

const recipePath = valueAfter('--recipe')
const recipe = JSON.parse(fs.readFileSync(recipePath, 'utf8'))
const recipeText = JSON.stringify(recipe)
for (const expected of ['wp-codebox.agent-sandbox-run', 'HOMEBOY_DATAMACHINE_AGENT_CONFIG', '/homeboy-extension', 'agents-api', 'data-machine', 'data-machine-code']) {
  if (!recipeText.includes(expected)) {
    throw new Error(`missing expected wp-codebox recipe value: ${expected}`)
  }
}
if (recipe.inputs?.extraPlugins?.some((plugin) => plugin.slug === 'php-ai-client')) {
  throw new Error('WP AI Client is provided by WordPress core and must not be mounted as a WP Codebox extra plugin')
}
for (const slug of ['agents-api', 'data-machine', 'data-machine-code']) {
  if (!recipe.inputs?.extraPlugins?.some((plugin) => plugin.slug === slug && plugin.activate === true)) {
    throw new Error(`expected ${slug} to be activated before the agent workload runs`)
  }
}
if (!recipe.inputs?.mounts?.some((mount) => mount.source.endsWith('/bundle') && mount.target === '/wordpress/wp-content/plugins/bundle' && mount.mode === 'readonly')) {
  throw new Error('missing bundle readonly mount in recipe')
}
if (!recipe.inputs?.mounts?.some((mount) => mount.type === 'file' && mount.source.endsWith('/fixture-config.json') && mount.target === '/wordpress/wp-content/plugins/example/fixture-config.json' && mount.mode === 'readonly')) {
  throw new Error('missing typed file mount in recipe')
}
if (!recipe.inputs?.mounts?.some((mount) => mount.type === 'directory' && mount.source.endsWith('/fixture-directory-mount') && mount.target === '/wordpress/wp-content/plugins/example/fixtures' && mount.mode === 'readwrite')) {
  throw new Error('missing typed directory mount in recipe')
}

const artifactRoot = path.join(valueAfter('--artifacts'), 'runtime-smoke')
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
fs.writeFileSync(path.join(filesRoot, 'runtime-reference-manifest.json'), JSON.stringify({
  schema: 'wp-codebox/runtime-reference-manifest-fixture/v1',
  runtime: { id: 'runtime-fixture', provider: 'wordpress-playground' },
  references: [{ kind: 'snapshot', digest: 'sha256:runtime-fixture' }],
  apiToken: 'fixture-secret-token',
}, null, 2) + '\n')
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
    eval_artifact: {
      run: [{ workflow_run_url: 'https://github.com/example/repo/actions/runs/456' }],
    },
    transcript_artifacts: [[{ json: '/tmp/wp-codebox-smoke-transcript.json', summary: 'Nested transcript fixture' }]],
    job_artifact_exports: [{ pr_url: 'https://github.com/example/repo/pull/123', branch: 'agent-artifacts/example' }],
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
  executions: [{
    command: 'wordpress.run-php',
    stdout: JSON.stringify({ output: JSON.stringify(output) }),
  }],
  artifacts: {
    directory: artifactRoot,
    manifestPath: path.join(artifactRoot, 'manifest.json'),
    metadataPath: path.join(artifactRoot, 'metadata.json'),
    blueprintAfterPath: path.join(artifactRoot, 'blueprint.after.json'),
    capturedMountsPath: path.join(filesRoot, 'mounted-files.json'),
    runtimeReferenceManifestPath: path.join(filesRoot, 'runtime-reference-manifest.json'),
    changedFilesPath: path.join(filesRoot, 'changed-files.json'),
    patchPath: path.join(filesRoot, 'patch.diff'),
    reviewPath: path.join(filesRoot, 'review.json'),
  },
}) + '\n')
NODE
chmod +x "$FAKE_WP_CODEBOX"

jq -n \
    --arg bundle "$BUNDLE_DIR" \
    --arg agentsApi "$AGENTS_API_DIR" \
    --arg dataMachine "$DATA_MACHINE_DIR" \
    --arg dataMachineCode "$DATA_MACHINE_CODE_DIR" \
    --arg fileMount "$FILE_MOUNT_PATH" \
    --arg dirMount "$DIR_MOUNT_PATH" \
    '{
        component_id: "wp-codebox-smoke-component",
        component_path: env.PWD,
        bundle_path: $bundle,
        agent_slug: "wp-codebox-smoke-agent",
        flow_slug: "wp-codebox-smoke-flow",
        workload_id: "wp-codebox-runner-smoke",
        workload_label: "WP Codebox runner smoke",
        dry_run: true,
        enable_terminal_actions: true,
        provider: "example-provider",
        model: "example-model",
        workspace_policy_check: {
            enabled: true,
            args: ["--scope", "smoke"]
        },
        prompt: "Smoke the WP Codebox runner adapter.",
        ability_tools: [{name: "fixture_tool", apiToken: "fixture-tool-secret"}],
        wp_codebox_components: {
            agents_api: $agentsApi,
            data_machine: $dataMachine,
            data_machine_code: $dataMachineCode
        },
        wp_codebox_mounts: [
            {type: "file", source: $fileMount, target: "/wordpress/wp-content/plugins/example/fixture-config.json", mode: "readonly"},
            ($dirMount + ":/wordpress/wp-content/plugins/example/fixtures:readwrite")
        ]
    }' > "$CONFIG_TMPFILE"

FAKE_WP_CODEBOX_ARGS_FILE="$FAKE_ARGS_FILE" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_WP_CODEBOX" \
HOMEBOY_DATAMACHINE_AGENT_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_DATAMACHINE_AGENT_REPLAY_BUNDLE_DIR="$REPLAY_BUNDLE_DIR" \
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
runtime_manifest_artifact=$(jq -r "$scenario | .artifacts.wp_codebox_runtime_reference_manifest.kind // \"missing\"" "$RESULTS_TMPFILE")
runtime_manifest_schema=$(jq -r "$scenario | .metadata.wp_codebox.runtime_reference_manifest.schema // \"missing\"" "$RESULTS_TMPFILE")
runtime_manifest_secret=$(jq -r "$scenario | .metadata.wp_codebox.runtime_reference_manifest.apiToken // \"missing\"" "$RESULTS_TMPFILE")
if [ "$wp_codebox_success" != "true" ] || [ -z "$artifact_dir" ] || [ "$review_schema" != "wp-codebox/artifact-review/v1" ] || [ "$changed_files_schema" != "wp-codebox/changed-files/v1" ] || [ "$review_artifact" != "review" ] || [ "$patch_artifact" != "patch" ] || [ "$runtime_manifest_artifact" != "runtime-reference-manifest" ] || [ "$runtime_manifest_schema" != "wp-codebox/runtime-reference-manifest-fixture/v1" ] || [ "$runtime_manifest_secret" != "[redacted]" ]; then
    echo "ERROR: wp_codebox metadata missing (success=$wp_codebox_success artifact_dir=$artifact_dir)" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi

runner_evidence_schema=$(jq -r "$scenario | .metadata.runner_evidence.schema // \"missing\"" "$RESULTS_TMPFILE")
runner_evidence_marker=$(jq -r "$scenario | .metadata.runner_evidence.redaction.marker // \"missing\"" "$RESULTS_TMPFILE")
runner_evidence_tool_secret=$(jq -r "$scenario | .metadata.runner_evidence.tool_surface.ability_tools[0].apiToken // \"missing\"" "$RESULTS_TMPFILE")
runner_evidence_runtime_manifest=$(jq -r "$scenario | .metadata.runner_evidence.runtime_surface.runtime_reference_manifest_available // false" "$RESULTS_TMPFILE")
if [ "$runner_evidence_schema" != "homeboy/datamachine-agent-runner-evidence/v1" ] || [ "$runner_evidence_marker" != "[redacted]" ] || [ "$runner_evidence_tool_secret" != "[redacted]" ] || [ "$runner_evidence_runtime_manifest" != "true" ]; then
    echo "ERROR: generic runner evidence missing or unredacted" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi

evidence_schema=$(jq -r "$scenario | .metadata.evidence_references.schema // \"missing\"" "$RESULTS_TMPFILE")
homeboy_result_path=$(jq -r "$scenario | .metadata.evidence_references.references.homeboy_result_json.path // \"\"" "$RESULTS_TMPFILE")
wp_codebox_bundle_available=$(jq -r "$scenario | .metadata.evidence_references.references.wp_codebox_artifact_bundle.available // false" "$RESULTS_TMPFILE")
runtime_trace_available=$(jq -r "$scenario | .metadata.evidence_references.references.runtime_episode_trace.available // false" "$RESULTS_TMPFILE")
replay_bundle_available=$(jq -r "$scenario | .metadata.evidence_references.references.replay_bundle_artifact.available // false" "$RESULTS_TMPFILE")
verifier_available=$(jq -r "$scenario | .metadata.evidence_references.references.artifact_verifier_result.available // false" "$RESULTS_TMPFILE")
policy_available=$(jq -r "$scenario | .metadata.evidence_references.references.workspace_policy_result.available // false" "$RESULTS_TMPFILE")
runtime_manifest_available=$(jq -r "$scenario | .metadata.evidence_references.references.wp_codebox_runtime_reference_manifest.available // false" "$RESULTS_TMPFILE")
transcript_path=$(jq -r "$scenario | .metadata.evidence_references.references.transcript_artifact.path // \"\"" "$RESULTS_TMPFILE")
pull_request_value=$(jq -r "$scenario | .metadata.evidence_references.references.pull_request.value // \"\"" "$RESULTS_TMPFILE")
workspace_branch_value=$(jq -r "$scenario | .metadata.evidence_references.references.workspace_branch.value // \"\"" "$RESULTS_TMPFILE")
workflow_run_path=$(jq -r "$scenario | .metadata.evidence_references.references.workflow_run.path // \"\"" "$RESULTS_TMPFILE")
trace_gap=$(jq -r "$scenario | any(.metadata.evidence_references.compatibility_gaps[]?; .field == \"runtime_episode_trace\")" "$RESULTS_TMPFILE")
if [ "$evidence_schema" != "homeboy/datamachine-agent-evidence-references/v1" ] || [ "$homeboy_result_path" != "$RESULTS_TMPFILE" ] || [ "$wp_codebox_bundle_available" != "true" ] || [ "$runtime_trace_available" != "true" ] || [ "$replay_bundle_available" != "true" ] || [ "$verifier_available" != "true" ] || [ "$policy_available" != "true" ] || [ "$runtime_manifest_available" != "true" ] || [ "$transcript_path" != "/tmp/wp-codebox-smoke-transcript.json" ] || [ "$pull_request_value" != "https://github.com/example/repo/pull/123" ] || [ "$workspace_branch_value" != "agent-artifacts/example" ] || [ "$workflow_run_path" != "https://github.com/example/repo/actions/runs/456" ] || [ "$trace_gap" != "false" ]; then
    echo "ERROR: stable evidence references missing or incomplete" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi

if ! grep -qx 'recipe-run' "$FAKE_ARGS_FILE" || ! grep -qx -- '--recipe' "$FAKE_ARGS_FILE" || ! grep -qx 'artifacts' "$FAKE_ARGS_FILE" || ! grep -qx 'workspace-policy' "$FAKE_ARGS_FILE"; then
    echo "ERROR: expected wp-codebox recipe-run invocation" >&2
    cat "$FAKE_ARGS_FILE" >&2
    exit 1
fi

echo "✓ WP Codebox Data Machine agent runner smoke test PASSED"
