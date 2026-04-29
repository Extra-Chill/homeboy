#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
RUNNER="${EXTENSION_PATH}/scripts/trace/trace-runner.sh"

SMOKE_ASSERTIONS=0

assert_contains() {
    local haystack="$1"
    local needle="$2"
    local label="$3"
    SMOKE_ASSERTIONS=$((SMOKE_ASSERTIONS + 1))
    case "$haystack" in
        *"$needle"*) ;;
        *)
            echo "FAIL: ${label} missing ${needle}" >&2
            echo "$haystack" >&2
            exit 1
            ;;
    esac
}

assert_file() {
    local path="$1"
    local label="$2"
    SMOKE_ASSERTIONS=$((SMOKE_ASSERTIONS + 1))
    if [ ! -f "$path" ]; then
        echo "FAIL: ${label} missing file ${path}" >&2
        exit 1
    fi
}

assert_json_field() {
    local path="$1"
    local expr="$2"
    local label="$3"
    SMOKE_ASSERTIONS=$((SMOKE_ASSERTIONS + 1))
    if ! php -r '
        $path = $argv[1];
        $expr = $argv[2];
        $data = json_decode(file_get_contents($path), true);
        if (!is_array($data)) { exit(2); }
        if ($expr === "status-pass" && ($data["status"] ?? null) === "pass") { exit(0); }
        if ($expr === "has-artifacts" && !empty($data["artifacts"]) && is_array($data["artifacts"])) { exit(0); }
        if ($expr === "has-timeline" && !empty($data["timeline"]) && is_array($data["timeline"])) { exit(0); }
        exit(1);
    ' "$path" "$expr"; then
        echo "FAIL: ${label} failed JSON assertion ${expr}" >&2
        exit 1
    fi
}

fixture="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-trace.XXXXXX")"
trap 'rm -rf "$fixture"' EXIT

mkdir -p "$fixture/traces" "$fixture/tests/traces" "$fixture/scripts/trace"

cat >"$fixture/traces/php-scenario.trace.php" <<'PHP'
<?php
$artifact_dir = getenv( 'HOMEBOY_TRACE_ARTIFACT_DIR' );
$results_file = getenv( 'HOMEBOY_TRACE_RESULTS_FILE' );
$artifact = $artifact_dir . '/php-artifact.txt';
file_put_contents( $artifact, 'php artifact' . PHP_EOL );
file_put_contents(
    $results_file,
    json_encode(
        array(
            'component_id' => getenv( 'HOMEBOY_COMPONENT_ID' ),
            'scenario_id'  => getenv( 'HOMEBOY_TRACE_SCENARIO' ),
            'status'       => 'pass',
            'summary'      => 'PHP trace scenario passed',
            'timeline'     => array(
                array(
                    't_ms'   => 0,
                    'source' => 'php',
                    'event'  => 'scenario.start',
                    'data'   => array(
                        'component_path' => getenv( 'HOMEBOY_COMPONENT_PATH' ),
                        'wp_cli'         => getenv( 'HOMEBOY_WP_CLI' ) ?: '',
                    ),
                ),
            ),
            'assertions'   => array(
                array(
                    'id'      => 'env-present',
                    'status'  => getenv( 'HOMEBOY_COMPONENT_PATH' ) ? 'pass' : 'fail',
                    'message' => 'component path env is available',
                ),
            ),
            'artifacts'    => array(
                array(
                    'label' => 'php artifact',
                    'path'  => $artifact,
                ),
            ),
        ),
        JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES
    ) . PHP_EOL
);
PHP

cat >"$fixture/tests/traces/test-scenario.trace.php" <<'PHP'
<?php
file_put_contents( getenv( 'HOMEBOY_TRACE_RESULTS_FILE' ), '{"status":"pass","timeline":[],"assertions":[],"artifacts":[]}' . PHP_EOL );
PHP

cat >"$fixture/scripts/trace/shell-scenario.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
artifact="$HOMEBOY_TRACE_ARTIFACT_DIR/shell-artifact.txt"
printf 'shell artifact\n' >"$artifact"
cat >"$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{
  "component_id": "${HOMEBOY_COMPONENT_ID}",
  "scenario_id": "${HOMEBOY_TRACE_SCENARIO}",
  "status": "pass",
  "summary": "Shell trace scenario passed",
  "timeline": [
    {"t_ms": 0, "source": "shell", "event": "scenario.start", "data": {"run_dir": "${HOMEBOY_RUN_DIR}"}}
  ],
  "assertions": [
    {"id": "artifact-dir", "status": "pass", "message": "artifact dir is available"}
  ],
  "artifacts": [
    {"label": "shell artifact", "path": "${artifact}"}
  ]
}
JSON
SH
chmod +x "$fixture/scripts/trace/shell-scenario.sh"

manifest="$(php -r '$json = json_decode(file_get_contents($argv[1]), true); echo $json["trace"]["extension_script"] ?? "";' "${EXTENSION_PATH}/wordpress.json")"
assert_contains "$manifest" "scripts/trace/trace-runner.sh" "manifest trace extension script"

list_output="$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" HOMEBOY_COMPONENT_PATH="$fixture" HOMEBOY_COMPONENT_ID="trace-fixture" HOMEBOY_TRACE_LIST_ONLY=1 bash "$RUNNER")"
assert_contains "$list_output" '"component_id": "trace-fixture"' "list mode envelope component"
assert_contains "$list_output" '"id": "php-scenario"' "list mode PHP scenario"
assert_contains "$list_output" '"source": "traces/php-scenario.trace.php"' "list mode PHP source"
assert_contains "$list_output" '"id": "test-scenario"' "list mode tests PHP scenario"
assert_contains "$list_output" '"id": "shell-scenario"' "list mode shell scenario"
assert_contains "$list_output" '"source": "scripts/trace/shell-scenario.sh"' "list mode shell source"

run_dir="$fixture/run"
php_results="$run_dir/php-results.json"
shell_results="$run_dir/shell-results.json"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_PATH="$fixture" \
HOMEBOY_COMPONENT_ID="trace-fixture" \
HOMEBOY_TRACE_SCENARIO="php-scenario" \
HOMEBOY_TRACE_RESULTS_FILE="$php_results" \
HOMEBOY_TRACE_ARTIFACT_DIR="$run_dir/artifacts/php" \
HOMEBOY_RUN_DIR="$run_dir" \
bash "$RUNNER"

assert_file "$php_results" "PHP scenario results"
assert_file "$run_dir/artifacts/php/php-artifact.txt" "PHP scenario artifact"
assert_file "$run_dir/artifacts/php/php-scenario.stdout.txt" "PHP stdout artifact"
assert_file "$run_dir/artifacts/php/php-scenario.stderr.txt" "PHP stderr artifact"
assert_file "$run_dir/artifacts/php/php-scenario.exit-code.txt" "PHP exit artifact"
assert_json_field "$php_results" "status-pass" "PHP scenario status"
assert_json_field "$php_results" "has-artifacts" "PHP scenario artifacts"
assert_json_field "$php_results" "has-timeline" "PHP scenario timeline"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_PATH="$fixture" \
HOMEBOY_COMPONENT_ID="trace-fixture" \
HOMEBOY_TRACE_SCENARIO="shell-scenario" \
HOMEBOY_TRACE_RESULTS_FILE="$shell_results" \
HOMEBOY_TRACE_ARTIFACT_DIR="$run_dir/artifacts/shell" \
HOMEBOY_RUN_DIR="$run_dir" \
bash "$RUNNER"

assert_file "$shell_results" "Shell scenario results"
assert_file "$run_dir/artifacts/shell/shell-artifact.txt" "Shell scenario artifact"
assert_file "$run_dir/artifacts/shell/shell-scenario.stdout.txt" "Shell stdout artifact"
assert_file "$run_dir/artifacts/shell/shell-scenario.stderr.txt" "Shell stderr artifact"
assert_file "$run_dir/artifacts/shell/shell-scenario.exit-code.txt" "Shell exit artifact"
assert_json_field "$shell_results" "status-pass" "Shell scenario status"
assert_json_field "$shell_results" "has-artifacts" "Shell scenario artifacts"
assert_json_field "$shell_results" "has-timeline" "Shell scenario timeline"

echo "WordPress trace runner smoke passed (${SMOKE_ASSERTIONS} assertions)."
