#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORDPRESS_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-static-source-smoke.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

SOURCE_ROOT="${TMP_ROOT}/wp-site-generator"
EXTRA_WORKLOAD_DIR="${TMP_ROOT}/rig-workloads"
EXTRA_WORKLOAD="${EXTRA_WORKLOAD_DIR}/rig-workload.php"
UNSELECTED_EXTRA_WORKLOAD="${EXTRA_WORKLOAD_DIR}/unselected-crash.php"
mkdir -p "${SOURCE_ROOT}/static-sites/demo" "${SOURCE_ROOT}/.github/homeboy" "${SOURCE_ROOT}/tests/bench" "${SOURCE_ROOT}/scenarios" "$EXTRA_WORKLOAD_DIR"
printf '<!doctype html><title>Demo</title>\n' > "${SOURCE_ROOT}/static-sites/demo/index.html"
printf '<?php return array();\n' > "${SOURCE_ROOT}/.github/homeboy/ssi-import-diagnostics.php"
printf '<?php return function (): array { return array(); };\n' > "${SOURCE_ROOT}/tests/bench/website-generation.php"
printf '<?php update_option("homeboy_bootstrap_fixture", "loaded", false);\n' > "${SOURCE_ROOT}/bootstrap.php"
printf '<?php return array("metrics" => array("scenario" => 1));\n' > "${SOURCE_ROOT}/scenarios/grader.php"
cat > "${SOURCE_ROOT}/scenarios/manifest.json" <<'JSON'
{
  "id": "manifest-navigation",
  "label": "Manifest navigation",
  "prompt": "Build a navigation scenario fixture.",
  "grader": "grader.php",
  "tags": ["smoke"],
  "metadata": {"fixture": "scenario-manifest"}
}
JSON
printf '<?php return function (): array { return array(); };\n' > "$EXTRA_WORKLOAD"
printf '<?php throw new RuntimeException("unselected workload executed");\n' > "$UNSELECTED_EXTRA_WORKLOAD"

RESOLVE_HELPER="${TMP_ROOT}/resolve-context.sh"
cat > "$RESOLVE_HELPER" <<'STUB'
homeboy_resolve_context() {
    PLUGIN_PATH="$HOMEBOY_SMOKE_SOURCE_ROOT"
    COMPONENT_ID="wp-site-generator"
}
STUB

BENCH_HELPER="${TMP_ROOT}/bench-helper.sh"
cat > "$BENCH_HELPER" <<'STUB'
homeboy_write_empty_bench_results() {
    printf '{"schema":"homeboy/bench-results/v1","benchmarks":[]}\n' > "$3"
}
STUB

RESULTS_ARTIFACTS_HELPER="${WORDPRESS_DIR}/scripts/bench/bench-results-artifacts.sh"
BROWSER_TARGET_HELPER="${TMP_ROOT}/browser-target.sh"
cat > "$BROWSER_TARGET_HELPER" <<'STUB'
homeboy_wordpress_emit_browser_target() {
    return 0
}
STUB

DEPENDENCY_HELPER="${TMP_ROOT}/validation-dependencies.sh"
cat > "$DEPENDENCY_HELPER" <<'STUB'
homeboy_export_validation_dependency_paths() {
    return 0
}
homeboy_get_validation_dependency_slug() {
    basename "$1"
}
homeboy_find_validation_dependency_plugin_main_file() {
    local plugin_path="${1:-}"
    if [ -f "${plugin_path}/packages/wordpress-plugin/wp-codebox.php" ]; then
        printf '%s\n' "${plugin_path}/packages/wordpress-plugin/wp-codebox.php"
        return 0
    fi
    return 1
}
STUB

CAPTURE_FILE="${TMP_ROOT}/capture.json"
WP_CODEBOX_BIN="${TMP_ROOT}/fixture-wp-codebox.js"
WP_CODEBOX_CORE_MODULE="${TMP_ROOT}/wp-codebox-core.mjs"
cat > "$WP_CODEBOX_CORE_MODULE" <<'STUB'
export function buildWordPressBenchRecipe(options) {
  const defines = options.wpConfigDefines || {};
  const blueprint = options.blueprint && typeof options.blueprint === 'object' && !Array.isArray(options.blueprint)
    ? {...options.blueprint, steps: [...(Array.isArray(options.blueprint.steps) ? options.blueprint.steps : [])]}
    : {steps: []};
  if (Object.keys(defines).length > 0) {
    blueprint.steps.push({step: 'defineWpConfigConsts', consts: defines});
  }
  return {
    schema: 'wp-codebox/workspace-recipe/v1',
    runtime: {wp: options.wordpressVersion, blueprint},
    inputs: {extraPlugins: options.extraPlugins || [], mounts: options.mounts || []},
    workflow: {steps: [{
      command: 'wordpress.bench',
      args: [
        `component-id=${options.componentId || options.pluginSlug}`,
        `plugin-slug=${options.pluginSlug}`,
        `iterations=${options.iterations || 3}`,
        `warmup=${options.warmupIterations ?? 1}`,
        `dependency-slugs=${(options.dependencySlugs || []).filter(Boolean).join(',')}`,
        `env-json=${JSON.stringify(options.env || {})}`,
        `bootstrap-files-json=${JSON.stringify(options.bootstrapFiles || [])}`,
        `workloads-json=${JSON.stringify(options.workloads || [])}`,
        `scenario-ids-json=${JSON.stringify(options.scenarioIds || [])}`,
      ],
    }]},
  };
}
STUB
cat > "$WP_CODEBOX_BIN" <<'STUB'
#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const recipeIndex = process.argv.indexOf('--recipe');
const recipePath = recipeIndex >= 0 ? process.argv[recipeIndex + 1] : '';
const recipe = recipePath ? JSON.parse(fs.readFileSync(recipePath, 'utf8')) : null;
const args = recipe?.workflow?.steps?.[0]?.args || [];
const workloadsArg = args.find((arg) => arg.startsWith('workloads-json='));
const workloads = workloadsArg ? JSON.parse(workloadsArg.slice('workloads-json='.length)) : [];
const scenarios = workloads.length > 0
  ? workloads.map((workload, index) => ({
    id: workload.id || `configured-${index}`,
    source: workload.source || 'config',
    iterations: 1,
    metrics: {},
    provenance: {workload_index: index},
  }))
  : [{
    id: 'rig-workload',
    source: 'in_tree',
    file: 'tests/bench/rig-workload.php',
    iterations: 1,
    metrics: {},
    provenance: {workload_file: 'tests/bench/rig-workload.php'}
  }];
fs.writeFileSync(process.env.HOMEBOY_SMOKE_CAPTURE_FILE, `${JSON.stringify({ argv: process.argv.slice(2), recipe }, null, 2)}\n`);
process.stdout.write(JSON.stringify({
  success: true,
  benchResults: {
    schema: 'homeboy/bench-results/v1',
    component_id: 'wp-site-generator',
    benchmarks: [],
    scenarios,
    warmup_iterations: 1
  }
}));
STUB
chmod +x "$WP_CODEBOX_BIN"

SETTINGS_JSON=$(jq -nc '{
    wp_config_defines: {HOMEBOY_FIXTURE_DEFINE: "yes"},
    bench_env: {HOMEBOY_FIXTURE_ENV: "yes"},
    wp_codebox_bootstrap_files: ["bootstrap.php"],
    wp_codebox_scenario_manifests: ["scenarios/manifest.json"],
    playground_blueprint: {
        steps: [
            {step: "installPlugin", pluginData: {resource: "git:directory", url: "https://github.com/chubes4/static-site-importer", ref: "main", refType: "branch"}, options: {activate: true, targetFolderName: "static-site-importer"}}
        ]
    },
    playground_workloads: [
        {
            id: "ssi-import",
            run: [
                {type: "wp-cli", command: "wp static-site-importer import-theme /wordpress/wp-content/plugins/wp-site-generator/static-sites/demo/index.html --slug=demo --format=json", parse: "json"},
                {type: "php", file: ".github/homeboy/ssi-import-diagnostics.php"}
            ]
        }
    ]
}')

HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_HELPER" \
HOMEBOY_RUNTIME_BENCH_HELPER_SH="$BENCH_HELPER" \
HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
HOMEBOY_SMOKE_SOURCE_ROOT="$SOURCE_ROOT" \
HOMEBOY_SMOKE_CAPTURE_FILE="$CAPTURE_FILE" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
HOMEBOY_BENCH_EXTRA_WORKLOADS="$EXTRA_WORKLOAD" \
HOMEBOY_WP_CODEBOX_BIN="$WP_CODEBOX_BIN" \
HOMEBOY_WP_CODEBOX_CORE_MODULE="$WP_CODEBOX_CORE_MODULE" \
HOMEBOY_BENCH_RESULTS_FILE="${TMP_ROOT}/bench-results.json" \
HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="${TMP_ROOT}/artifacts" \
HOMEBOY_RUNTIME_FAILURE_TRAP="" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_BENCH_WARMUP_ITERATIONS=0 \
bash "$SCRIPT_DIR/bench-runner-wp-codebox.sh" >/dev/null

jq -e --arg sourceRoot "$SOURCE_ROOT" '
    .recipe.inputs.extraPlugins == []
    and (.recipe.inputs.mounts[] | select(.source == $sourceRoot and .target == "/wordpress/wp-content/plugins/wp-site-generator" and .mode == "readonly"))
    and (.recipe.inputs.mounts[] | select(.source | endswith("/rig-workloads/rig-workload.php")) | .target == "/wordpress/wp-content/plugins/wp-site-generator/.homeboy/bench-rig/rig-workload.php")
    and (.recipe.runtime.blueprint.steps[] | select(.step == "installPlugin" and .options.targetFolderName == "static-site-importer"))
    and (.recipe.runtime.blueprint.steps[] | select(.step == "defineWpConfigConsts" and .consts.HOMEBOY_FIXTURE_DEFINE == "yes"))
    and (.recipe.workflow.steps[0].args[] | select(startswith("env-json=") and contains("HOMEBOY_FIXTURE_ENV")))
    and (.recipe.workflow.steps[0].args[] | select(startswith("bootstrap-files-json=") and contains("bootstrap.php")))
    and (.recipe.workflow.steps[0].args[] | select(startswith("workloads-json=") and contains("static-site-importer import-theme")))
    and (.recipe.workflow.steps[0].args[] | select(startswith("workloads-json=") and contains("manifest-navigation") and contains("scenario-manifest")))
' "$CAPTURE_FILE" >/dev/null

SELECTED_CAPTURE_FILE="${TMP_ROOT}/selected-extra-workload-capture.json"
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_HELPER" \
HOMEBOY_RUNTIME_BENCH_HELPER_SH="$BENCH_HELPER" \
HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
HOMEBOY_SMOKE_SOURCE_ROOT="$SOURCE_ROOT" \
HOMEBOY_SMOKE_CAPTURE_FILE="$SELECTED_CAPTURE_FILE" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
HOMEBOY_BENCH_EXTRA_WORKLOADS="${EXTRA_WORKLOAD}:${UNSELECTED_EXTRA_WORKLOAD}" \
HOMEBOY_BENCH_SCENARIOS="rig-workload" \
HOMEBOY_WP_CODEBOX_BIN="$WP_CODEBOX_BIN" \
HOMEBOY_WP_CODEBOX_CORE_MODULE="$WP_CODEBOX_CORE_MODULE" \
HOMEBOY_BENCH_RESULTS_FILE="${TMP_ROOT}/selected-extra-workload-results.json" \
HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="${TMP_ROOT}/selected-extra-workload-artifacts" \
HOMEBOY_RUNTIME_FAILURE_TRAP="" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_BENCH_WARMUP_ITERATIONS=0 \
bash "$SCRIPT_DIR/bench-runner-wp-codebox.sh" >/dev/null

jq -e '
    ([.recipe.inputs.mounts[] | select(.source | endswith("/rig-workloads/rig-workload.php"))] | length) == 1
    and ([.recipe.inputs.mounts[] | select(.source | endswith("/rig-workloads/unselected-crash.php"))] | length) == 0
    and ([.recipe.inputs.mounts[] | select(.target == "/wordpress/wp-content/plugins/wp-site-generator/tests/bench")] | length) == 0
    and ([.recipe.workflow.steps[0].args[] | select(startswith("workloads-json=") and contains("ssi-import"))] | length) == 0
    and ([.recipe.workflow.steps[0].args[] | select(startswith("workloads-json=") and contains("\"source\":\"rig\"") and contains(".homeboy/bench-rig/rig-workload.php"))] | length) == 1
    and ([.recipe.workflow.steps[0].args[] | select(startswith("scenario-ids-json=") and contains("rig-workload"))] | length) == 1
' "$SELECTED_CAPTURE_FILE" >/dev/null

printf '<?php return function (): array { return array("metrics" => array("shadow" => 1)); };\n' > "${SOURCE_ROOT}/tests/bench/rig-workload.php"
DUPLICATE_CAPTURE_FILE="${TMP_ROOT}/duplicate-capture.json"
DUPLICATE_RESULTS_FILE="${TMP_ROOT}/duplicate-results.json"
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_HELPER" \
HOMEBOY_RUNTIME_BENCH_HELPER_SH="$BENCH_HELPER" \
HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
HOMEBOY_SMOKE_SOURCE_ROOT="$SOURCE_ROOT" \
HOMEBOY_SMOKE_CAPTURE_FILE="$DUPLICATE_CAPTURE_FILE" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
HOMEBOY_BENCH_EXTRA_WORKLOADS="$EXTRA_WORKLOAD" \
HOMEBOY_BENCH_SCENARIOS="rig-workload" \
HOMEBOY_WP_CODEBOX_BIN="$WP_CODEBOX_BIN" \
HOMEBOY_WP_CODEBOX_CORE_MODULE="$WP_CODEBOX_CORE_MODULE" \
HOMEBOY_BENCH_RESULTS_FILE="$DUPLICATE_RESULTS_FILE" \
HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="${TMP_ROOT}/duplicate-artifacts" \
HOMEBOY_RUNTIME_FAILURE_TRAP="" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_BENCH_WARMUP_ITERATIONS=0 \
bash "$SCRIPT_DIR/bench-runner-wp-codebox.sh" >/dev/null
rm -f "${SOURCE_ROOT}/tests/bench/rig-workload.php"
jq -e --arg sourceRoot "$SOURCE_ROOT" '
    (.recipe.inputs.mounts[] | select(.source | endswith("/rig-workloads/rig-workload.php") and .target == "/wordpress/wp-content/plugins/wp-site-generator/.homeboy/bench-rig/rig-workload.php"))
' "$DUPLICATE_CAPTURE_FILE" >/dev/null
jq -e '
    (.scenarios[] | select(.id == "rig-workload" and .source == "rig" and .provenance.workload_index == 0))
' "$DUPLICATE_RESULTS_FILE" >/dev/null

LIST_RESULTS_FILE="${TMP_ROOT}/bench-list-results.json"
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_HELPER" \
HOMEBOY_RUNTIME_BENCH_HELPER_SH="$BENCH_HELPER" \
HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
HOMEBOY_SMOKE_SOURCE_ROOT="$SOURCE_ROOT" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
HOMEBOY_BENCH_EXTRA_WORKLOADS="$EXTRA_WORKLOAD" \
HOMEBOY_BENCH_RESULTS_FILE="$LIST_RESULTS_FILE" \
HOMEBOY_RUNTIME_FAILURE_TRAP="" \
HOMEBOY_BENCH_LIST_ONLY=1 \
bash "$SCRIPT_DIR/bench-runner-wp-codebox.sh" >/dev/null

jq -e '
    .component_id == "wp-site-generator"
    and .iterations == 0
    and (.scenarios[] | select(.id == "website-generation" and .file == "tests/bench/website-generation.php" and .source == "component"))
    and (.scenarios[] | select(.id == "rig-workload" and .file == "tests/bench/rig-workload.php" and .source == "rig"))
    and (.scenarios[] | select(.id == "ssi-import" and .source == "configured"))
    and (.scenarios[] | select(.id == "manifest-navigation" and .source == "scenario-manifest"))
' "$LIST_RESULTS_FILE" >/dev/null

PLUGIN_ROOT="${TMP_ROOT}/plugin-component"
mkdir -p "$PLUGIN_ROOT/scenarios"
printf '<?php\n/**\n * Plugin Name: Fixture Component\n */\n' > "${PLUGIN_ROOT}/plugin-main.php"
printf '<?php update_option("homeboy_bootstrap_fixture", "loaded", false);\n' > "${PLUGIN_ROOT}/bootstrap.php"
printf '<?php return array("metrics" => array("scenario" => 1));\n' > "${PLUGIN_ROOT}/scenarios/grader.php"
cat > "${PLUGIN_ROOT}/scenarios/manifest.json" <<'JSON'
{
  "id": "manifest-navigation",
  "label": "Manifest navigation",
  "prompt": "Build a navigation scenario fixture.",
  "grader": "grader.php",
  "tags": ["smoke"],
  "metadata": {"fixture": "scenario-manifest"}
}
JSON
PLUGIN_CAPTURE_FILE="${TMP_ROOT}/plugin-capture.json"

HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_HELPER" \
HOMEBOY_RUNTIME_BENCH_HELPER_SH="$BENCH_HELPER" \
HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
HOMEBOY_SMOKE_SOURCE_ROOT="$PLUGIN_ROOT" \
HOMEBOY_SMOKE_CAPTURE_FILE="$PLUGIN_CAPTURE_FILE" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
HOMEBOY_WP_CODEBOX_BIN="$WP_CODEBOX_BIN" \
HOMEBOY_WP_CODEBOX_CORE_MODULE="$WP_CODEBOX_CORE_MODULE" \
HOMEBOY_BENCH_RESULTS_FILE="${TMP_ROOT}/plugin-bench-results.json" \
HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="${TMP_ROOT}/plugin-artifacts" \
HOMEBOY_RUNTIME_FAILURE_TRAP="" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_BENCH_WARMUP_ITERATIONS=0 \
bash "$SCRIPT_DIR/bench-runner-wp-codebox.sh" >/dev/null

jq -e --arg sourceRoot "$PLUGIN_ROOT" '
    .recipe.inputs.extraPlugins == [{source: $sourceRoot, slug: "wp-site-generator", pluginFile: "wp-site-generator/plugin-main.php", activate: false}]
    and ([.recipe.inputs.mounts[] | select(.source == $sourceRoot and .target == "/wordpress/wp-content/plugins/wp-site-generator")] | length == 0)
' "$PLUGIN_CAPTURE_FILE" >/dev/null

mkdir -p "${PLUGIN_ROOT}/tests/bench"
printf '<?php return function (): array { return array("metrics" => array("shadow" => 1)); };\n' > "${PLUGIN_ROOT}/tests/bench/rig-workload.php"
PLUGIN_DUPLICATE_CAPTURE_FILE="${TMP_ROOT}/plugin-duplicate-capture.json"
PLUGIN_DUPLICATE_RESULTS_FILE="${TMP_ROOT}/plugin-duplicate-results.json"
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_HELPER" \
HOMEBOY_RUNTIME_BENCH_HELPER_SH="$BENCH_HELPER" \
HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
HOMEBOY_SMOKE_SOURCE_ROOT="$PLUGIN_ROOT" \
HOMEBOY_SMOKE_CAPTURE_FILE="$PLUGIN_DUPLICATE_CAPTURE_FILE" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
HOMEBOY_BENCH_EXTRA_WORKLOADS="$EXTRA_WORKLOAD" \
HOMEBOY_BENCH_SCENARIOS="rig-workload" \
HOMEBOY_WP_CODEBOX_BIN="$WP_CODEBOX_BIN" \
HOMEBOY_WP_CODEBOX_CORE_MODULE="$WP_CODEBOX_CORE_MODULE" \
HOMEBOY_BENCH_RESULTS_FILE="$PLUGIN_DUPLICATE_RESULTS_FILE" \
HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="${TMP_ROOT}/plugin-duplicate-artifacts" \
HOMEBOY_RUNTIME_FAILURE_TRAP="" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_BENCH_WARMUP_ITERATIONS=0 \
bash "$SCRIPT_DIR/bench-runner-wp-codebox.sh" >/dev/null

jq -e '
    .recipe.inputs.extraPlugins[0].pluginFile == "wp-site-generator/plugin-main.php"
    and (.recipe.inputs.mounts[] | select(.source | endswith("/rig-workloads/rig-workload.php") and .target == "/wordpress/wp-content/plugins/wp-site-generator/.homeboy/bench-rig/rig-workload.php"))
    and ([.recipe.workflow.steps[0].args[] | select(startswith("workloads-json=") and contains("\"source\":\"rig\"") and contains(".homeboy/bench-rig/rig-workload.php"))] | length) == 1
    and ([.recipe.workflow.steps[0].args[] | select(startswith("scenario-ids-json=") and contains("rig-workload"))] | length) == 1
' "$PLUGIN_DUPLICATE_CAPTURE_FILE" >/dev/null
jq -e '
    (.scenarios[] | select(.id == "rig-workload" and .source == "rig" and .provenance.workload_index == 0))
' "$PLUGIN_DUPLICATE_RESULTS_FILE" >/dev/null

DEPENDENCY_ROOT="${TMP_ROOT}/wp-codebox-release-fixture"
mkdir -p "${DEPENDENCY_ROOT}/packages/wordpress-plugin"
printf '<?php\n/**\n * Plugin Name: WP Codebox Fixture\n */\n' > "${DEPENDENCY_ROOT}/packages/wordpress-plugin/wp-codebox.php"

DEPENDENCY_CAPTURE_FILE="${TMP_ROOT}/dependency-capture.json"
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_HELPER" \
HOMEBOY_RUNTIME_BENCH_HELPER_SH="$BENCH_HELPER" \
HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
HOMEBOY_WORDPRESS_DEPENDENCY_PATHS="$DEPENDENCY_ROOT" \
HOMEBOY_SMOKE_SOURCE_ROOT="$PLUGIN_ROOT" \
HOMEBOY_SMOKE_CAPTURE_FILE="$DEPENDENCY_CAPTURE_FILE" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
HOMEBOY_WP_CODEBOX_BIN="$WP_CODEBOX_BIN" \
HOMEBOY_WP_CODEBOX_CORE_MODULE="$WP_CODEBOX_CORE_MODULE" \
HOMEBOY_BENCH_RESULTS_FILE="${TMP_ROOT}/dependency-bench-results.json" \
HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="${TMP_ROOT}/dependency-artifacts" \
HOMEBOY_RUNTIME_FAILURE_TRAP="" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_BENCH_WARMUP_ITERATIONS=0 \
bash "$SCRIPT_DIR/bench-runner-wp-codebox.sh" >/dev/null

jq -e --arg pluginRoot "$PLUGIN_ROOT" --arg dependencyRoot "${DEPENDENCY_ROOT}/packages/wordpress-plugin" '
    .recipe.inputs.extraPlugins == [
        {source: $pluginRoot, slug: "wp-site-generator", pluginFile: "wp-site-generator/plugin-main.php", activate: false},
        {source: $dependencyRoot, slug: "wp-codebox", pluginFile: "wp-codebox/wp-codebox.php", activate: false}
    ]
' "$DEPENDENCY_CAPTURE_FILE" >/dev/null

echo "WP Codebox static-source bench smoke passed"
