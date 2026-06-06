#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

PLUGIN_PATH="${TMPDIR}/component"
FAKE_WP_CODEBOX="${TMPDIR}/wp-codebox.js"
FAKE_CORE_MODULE="${TMPDIR}/wp-codebox-core.mjs"
FAKE_PREFLIGHT="${TMPDIR}/bash-preflight.sh"
FAKE_BENCH_HELPER="${TMPDIR}/bench-helper.sh"
CAPTURED_RECIPE="${TMPDIR}/captured-recipe.json"
CAPTURED_RECIPE_PATH="${TMPDIR}/captured-recipe-path.txt"
RESULTS_FILE="${TMPDIR}/bench-results.json"
ARTIFACTS_DIR="${TMPDIR}/artifacts"

mkdir -p "${PLUGIN_PATH}/tests/bench"
printf '<?php\nreturn array( "metrics" => array( "noop" => 1 ) );\n' > "${PLUGIN_PATH}/tests/bench/noop.php"
cat > "$FAKE_PREFLIGHT" <<'SH'
homeboy_require_bash_version() { :; }
SH
cat > "$FAKE_BENCH_HELPER" <<'SH'
homeboy_write_empty_bench_results() { :; }
SH

cat > "$FAKE_CORE_MODULE" <<'NODE'
export function buildWordPressBenchRecipe(options) {
  return {
    schema: "wp-codebox/workspace-recipe/v1",
    runtime: { wp: options.wordpressVersion, blueprint: options.blueprint },
    inputs: { mounts: options.mounts ?? [] },
    workflow: { steps: [{ command: "wordpress.bench", args: [`plugin-slug=${options.pluginSlug}`] }] },
  }
}
NODE

cat > "$FAKE_WP_CODEBOX" <<'NODE'
#!/usr/bin/env node
import { copyFileSync, writeFileSync } from "node:fs"

const recipeIndex = process.argv.indexOf("--recipe")
if (recipeIndex !== -1 && process.argv[recipeIndex + 1]) {
  copyFileSync(process.argv[recipeIndex + 1], process.env.CAPTURED_RECIPE)
  writeFileSync(process.env.CAPTURED_RECIPE_PATH, process.argv[recipeIndex + 1])
}
writeFileSync(1, 'PHP.run() failed with exit code 255.\n\nFatal error: Allowed memory size of 268435456 bytes exhausted (tried to allocate 126976 bytes) in /internal/shared/sqlite-database-integration/wp-includes/database/sqlite/class-wp-pdo-proxy-statement.php on line 358\n')
process.exit(255)
NODE
chmod +x "$FAKE_WP_CODEBOX"

set +e
output=$(CAPTURED_RECIPE="$CAPTURED_RECIPE" \
    CAPTURED_RECIPE_PATH="$CAPTURED_RECIPE_PATH" \
    HOMEBOY_WP_CODEBOX_BIN="$FAKE_WP_CODEBOX" \
    HOMEBOY_WP_CODEBOX_CORE_MODULE="$FAKE_CORE_MODULE" \
    HOMEBOY_RUNTIME_BASH_PREFLIGHT="$FAKE_PREFLIGHT" \
    HOMEBOY_RUNTIME_BENCH_HELPER_SH="$FAKE_BENCH_HELPER" \
    HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_FILE" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
    HOMEBOY_COMPONENT_ID="bench-memory-budget" \
    HOMEBOY_SETTINGS_JSON="{\"wp_codebox_php_memory_limit\":\"768M\",\"wp_codebox_artifacts_dir\":\"${ARTIFACTS_DIR}\"}" \
    bash "${SCRIPT_DIR}/bench-runner.sh" 2>&1)
status=$?
set -e

if [ "$status" -ne 255 ]; then
    echo "Expected fake WP Codebox exit 255, got $status" >&2
    echo "$output" >&2
    exit 1
fi

if ! jq -e '.inputs.pluginRuntime.php.memoryLimit == "768M"' "$CAPTURED_RECIPE" >/dev/null; then
    echo "Expected generated recipe to include pluginRuntime.php.memoryLimit=768M" >&2
    cat "$CAPTURED_RECIPE" >&2
    exit 1
fi

if ! jq -e '.runtime.wp == "latest"' "$CAPTURED_RECIPE" >/dev/null; then
    echo "Expected generated recipe to use a valid default WordPress runtime" >&2
    cat "$CAPTURED_RECIPE" >&2
    exit 1
fi

captured_recipe_path=$(cat "$CAPTURED_RECIPE_PATH")
case "$captured_recipe_path" in
    "$ARTIFACTS_DIR"/*) ;;
    *)
        echo "Expected generated recipe scratch file to live under artifacts directory" >&2
        echo "$captured_recipe_path" >&2
        exit 1
        ;;
esac

DIAGNOSTICS_FILE="${ARTIFACTS_DIR}/wp-codebox-bench-diagnostics.json"
if ! jq -e '
    .schema == "homeboy/wordpress-bench-diagnostic/v1"
    and .diagnostics[0].code == "wp-codebox-php-memory-exhausted"
    and .diagnostics[0].php_memory_limit == "768M"
    and .diagnostics[0].exit_code == 255
    and .diagnostics[0].allowed_bytes == 268435456
    and .diagnostics[0].attempted_allocation_bytes == 126976
    and .diagnostics[0].failing_file == "/internal/shared/sqlite-database-integration/wp-includes/database/sqlite/class-wp-pdo-proxy-statement.php"
    and .diagnostics[0].failing_line == 358
' "$DIAGNOSTICS_FILE" >/dev/null; then
    echo "Expected structured memory fatal diagnostics" >&2
    echo "$output" >&2
    [ -f "$DIAGNOSTICS_FILE" ] && cat "$DIAGNOSTICS_FILE" >&2
    exit 1
fi

if [[ "$output" != *"WP Codebox wordpress.bench exhausted PHP memory"* ]]; then
    echo "Expected memory fatal summary in output" >&2
    echo "$output" >&2
    exit 1
fi

echo "WP Codebox bench memory budget smoke passed"
