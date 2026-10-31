#!/usr/bin/env bash
set -euo pipefail

# Regression: the PHPStan single-process (--debug) retry must hand the full
# PHPStan JSON report to the summary and the lint-findings sidecar.
#
# Extra-Chill/homeboy-extensions#2903: on a large component (data-machine at
# level 7) the parallel run failed with
#
#     Child process error: PHPStan process crashed because it reached
#     configured PHP memory limit: 2G ... while running parallel worker
#
# The runner retried with `--debug`. That run died on the same 2G limit before
# it ever printed a report: stdout held only the analysed-file-path lines, the
# real error sat in stderr. The runner then substituted the raw path list for
# the missing JSON, so the summary printed a file listing as "errors" and the
# findings sidecar was written as `[]` — every PHPStan finding silently dropped
# and the baseline came out PHPCS-only.
#
# This smoke stubs the phpstan binary so the FIRST run always reports a
# parallel-worker failure and the `--debug` retry emits the shapes PHPStan
# produces, then asserts:
#   - a report that follows `--debug` path lines (own line, glued onto the last
#     path with no newline, or followed by trailing output) reaches the sidecar;
#   - a retry that dies without a report records NO findings, surfaces the real
#     stderr error, and never dresses the path list up as findings;
#   - the memory limit is configurable and shared by run and retry.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

# Copy the extension so vendor/bin/phpstan can be replaced with a stub without
# touching the real install. The runner resolves ROOT_DIR two levels above its
# own scripts/lint directory, so mirror that layout.
EXT_DIR="${TMP_ROOT}/ext"
mkdir -p "${EXT_DIR}/wordpress" "${EXT_DIR}/scripts"
tar -C "$ROOT_DIR" --exclude=./vendor --exclude=./node_modules --exclude=./tests -cf - . \
    | tar -C "${EXT_DIR}/wordpress" -xf -
cp -R "${ROOT_DIR}/../scripts/lib" "${EXT_DIR}/scripts/lib"
EXT_WP="${EXT_DIR}/wordpress"

COMPONENT_DIR="${TMP_ROOT}/component"
CONFIG_TMPDIR="${TMP_ROOT}/generated-config"
CALL_LOG="${TMP_ROOT}/phpstan-calls.log"
SIDECAR_FILE="${TMP_ROOT}/findings.json"
OUTPUT_FILE="${TMP_ROOT}/runner-output.txt"
mkdir -p "${COMPONENT_DIR}/inc" "$CONFIG_TMPDIR" "${EXT_WP}/vendor/bin"

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: PHPStan Retry Findings Fixture
 */
PHP
cat > "${COMPONENT_DIR}/inc/alpha.php" <<'PHP'
<?php
function fixture_alpha( int $value ): string {
    return $value;
}
PHP
cat > "${COMPONENT_DIR}/inc/beta.php" <<'PHP'
<?php
function fixture_beta(): void {
    fixture_missing_function();
}
PHP

cat > "${TMP_ROOT}/resolve-context.sh" <<'SH'
homeboy_resolve_context() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-phpstan-retry-findings}"
}
SH
cat > "${TMP_ROOT}/sidecar-writer.sh" <<'SH'
# Minimal stand-in for the real writer: the runner hands over (target, source).
homeboy_sidecar_merge_json_array() {
    cp "$2" "$1"
}
SH

# Stub PHPStan. Behaviour is driven by FIXTURE_RETRY_SHAPE for the --debug
# retry; the first (parallel) run always reports a parallel-worker failure the
# way PHPStan does: exit 1, a JSON report with only a global error on stdout.
cat > "${EXT_WP}/vendor/bin/phpstan" <<'SH'
#!/usr/bin/env bash
printf 'cwd=%s args=%s\n' "$PWD" "$*" >> "$FIXTURE_CALL_LOG"

if [ "$1" = "--version" ]; then
    echo "PHPStan - PHP Static Analysis Tool (stub)"
    exit 0
fi

debug=0
for arg in "$@"; do [ "$arg" = "--debug" ] && debug=1; done

component="$FIXTURE_COMPONENT_DIR"

report=$(cat <<JSON
{"totals":{"errors":0,"file_errors":2},"files":{"${component}/inc/alpha.php":{"errors":1,"messages":[{"message":"Function fixture_alpha() should return string but returns int.","line":3,"ignorable":true,"identifier":"return.type"}]},"${component}/inc/beta.php":{"errors":1,"messages":[{"message":"Function fixture_missing_function not found.","line":3,"ignorable":true,"identifier":"function.notFound"}]}},"errors":[]}
JSON
)

if [ "$debug" -eq 0 ]; then
    # Parallel run: worker crash, no per-file findings. Newlines stay escaped
    # (\n) because this is JSON, exactly as PHPStan prints it.
    printf '%s\n' '{"totals":{"errors":1,"file_errors":0},"files":{},"errors":["Child process error: PHPStan process crashed because it reached configured PHP memory limit: 2G\nIncrease your memory limit in php.ini or run PHPStan with --memory-limit CLI option.\n while running parallel worker"]}'
    # Text-format runs (no --error-format=json) print the box instead.
    if ! printf '%s ' "$@" | grep -q -- '--error-format=json'; then
        echo "Child process error: ... while running parallel worker"
    fi
    exit 1
fi

# --debug retry: one analysed path per line first, like real PHPStan.
printf '%s\n' "${component}/inc/alpha.php" "${component}/plugin.php"
case "${FIXTURE_RETRY_SHAPE:-own-line}" in
    own-line)
        printf '%s\n' "${component}/inc/beta.php"
        printf '%s\n' "$report"
        exit 1
        ;;
    glued)
        # The last path is not newline-terminated, so the report shares its line.
        printf '%s' "${component}/inc/beta.php"
        printf '%s\n' "$report"
        exit 1
        ;;
    trailing-output)
        printf '%s\n' "${component}/inc/beta.php"
        printf '%s\n' "$report"
        printf '%s\n' "Notice: trailing output after the report"
        exit 1
        ;;
    no-report)
        # Died mid-analysis on the memory limit: paths only, no report.
        printf '%s\n' "${component}/inc/beta.php"
        echo "PHP Fatal error:  Allowed memory size of 2147483648 bytes exhausted (tried to allocate 4096 bytes)" >&2
        echo "PHPStan process crashed because it reached configured PHP memory limit: 2G" >&2
        exit 255
        ;;
esac
SH
chmod +x "${EXT_WP}/vendor/bin/phpstan"

run_runner() {
    # Usage: run_runner [ENV=VALUE ...]
    set +e
    : > "$CALL_LOG"
    rm -f "$SIDECAR_FILE"
    env \
        HOMEBOY_EXTENSION_PATH="$EXT_WP" \
        HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
        HOMEBOY_COMPONENT_ID="phpstan-retry-findings" \
        HOMEBOY_RUNTIME_RESOLVE_CONTEXT="${TMP_ROOT}/resolve-context.sh" \
        HOMEBOY_RUNTIME_SIDECAR_WRITER="${TMP_ROOT}/sidecar-writer.sh" \
        HOMEBOY_PHPSTAN_THREADS=1 \
        FIXTURE_CALL_LOG="$CALL_LOG" \
        FIXTURE_COMPONENT_DIR="$COMPONENT_DIR" \
        _HOMEBOY_PHPSTAN_FINDINGS_FILE="$SIDECAR_FILE" \
        TMPDIR="$CONFIG_TMPDIR" \
        "$@" \
        bash -c 'cd "$1" && "$2"' _ "$CONFIG_TMPDIR" "${EXT_WP}/scripts/lint/phpstan-runner.sh" >"$OUTPUT_FILE" 2>&1
    RUNNER_EXIT=$?
    set -e
}

fail() {
    echo "FAIL: $1" >&2
    echo "--- runner output ---" >&2
    cat "$OUTPUT_FILE" >&2
    echo "--- phpstan calls ---" >&2
    cat "$CALL_LOG" >&2
    exit 1
}

assert_retried() {
    grep -q "Parallel worker failure detected, retrying single-process" "$OUTPUT_FILE" \
        || fail "$1: the first run should have been classified as a parallel-worker failure"
    [ "$(grep -c -- '--debug' "$CALL_LOG")" -eq 1 ] || fail "$1: exactly one --debug retry expected"
}

sidecar_count() {
    php -r '
        $findings = json_decode(file_get_contents($argv[1]), true);
        echo is_array($findings) ? count($findings) : "invalid";
    ' "$SIDECAR_FILE"
}

sidecar_codes() {
    php -r '
        $findings = json_decode(file_get_contents($argv[1]), true);
        $codes = array_map(static fn ($f) => $f["file"] . "=" . $f["code"], $findings);
        sort($codes);
        echo implode(",", $codes);
    ' "$SIDECAR_FILE"
}

assert_findings_reach_sidecar() {
    local label="$1"

    assert_retried "$label"

    [ -f "$SIDECAR_FILE" ] || fail "${label}: findings sidecar was not written"

    local count
    count="$(sidecar_count)"
    [ "$count" = "2" ] || fail "${label}: sidecar should hold 2 PHPStan findings, got '${count}'"

    [ "$(sidecar_codes)" = "inc/alpha.php=phpstan.return.type,inc/beta.php=phpstan.function.notFound" ] \
        || fail "${label}: sidecar findings do not match the PHPStan report: $(sidecar_codes)"

    grep -q "PHPSTAN SUMMARY: 2 errors" "$OUTPUT_FILE" \
        || fail "${label}: summary should report the 2 PHPStan errors"
    if grep -q "ERRORS (raw)" "$OUTPUT_FILE"; then
        fail "${label}: summary printed the raw debug stream instead of parsing the report"
    fi
    [ "$RUNNER_EXIT" -eq 1 ] || fail "${label}: analysis with findings should exit 1, got ${RUNNER_EXIT}"
}

# --- Report shapes that follow the --debug path lines ------------------------
for shape in own-line glued trailing-output; do
    run_runner FIXTURE_RETRY_SHAPE="$shape" HOMEBOY_SUMMARY_MODE=1
    assert_findings_reach_sidecar "summary/${shape}"
done

# --- Retry dies without a report ---------------------------------------------
run_runner FIXTURE_RETRY_SHAPE=no-report HOMEBOY_SUMMARY_MODE=1
assert_retried "summary/no-report"
[ "$RUNNER_EXIT" -ne 0 ] || fail "summary/no-report: a run with no report must not pass"
if [ -f "$SIDECAR_FILE" ]; then
    fail "summary/no-report: no findings sidecar should be written when PHPStan produced no report (got: $(cat "$SIDECAR_FILE"))"
fi
if grep -q "ERRORS (raw)" "$OUTPUT_FILE" || grep -q "PHPSTAN SUMMARY" "$OUTPUT_FILE"; then
    fail "summary/no-report: the debug path list must not be presented as findings"
fi
grep -q "without producing a JSON report" "$OUTPUT_FILE" \
    || fail "summary/no-report: should say no report was produced"
grep -q "reached configured PHP memory limit" "$OUTPUT_FILE" \
    || fail "summary/no-report: should surface the retry's real stderr error"
grep -q "HOMEBOY_PHPSTAN_MEMORY_LIMIT" "$OUTPUT_FILE" \
    || fail "summary/no-report: should point at the memory limit override"

# --- Memory limit is configurable and shared by run and retry ---------------
run_runner FIXTURE_RETRY_SHAPE=own-line HOMEBOY_SUMMARY_MODE=1
[ "$(grep -c -- '--memory-limit=4G' "$CALL_LOG")" -eq 2 ] \
    || fail "default memory limit (4G) should apply to both the run and the retry"

run_runner FIXTURE_RETRY_SHAPE=own-line HOMEBOY_SUMMARY_MODE=1 HOMEBOY_PHPSTAN_MEMORY_LIMIT=6G
[ "$(grep -c -- '--memory-limit=6G' "$CALL_LOG")" -eq 2 ] \
    || fail "HOMEBOY_PHPSTAN_MEMORY_LIMIT should apply to both the run and the retry"

# --- Non-summary (text report) retry -----------------------------------------
run_runner FIXTURE_RETRY_SHAPE=no-report
assert_retried "full/no-report"
[ "$RUNNER_EXIT" -ne 0 ] || fail "full/no-report: a run with no report must not pass"
grep -q "reached configured PHP memory limit" "$OUTPUT_FILE" \
    || fail "full/no-report: should surface the retry's real stderr error"
grep -q "HOMEBOY_PHPSTAN_MEMORY_LIMIT" "$OUTPUT_FILE" \
    || fail "full/no-report: should point at the memory limit override"
[ "$(grep -c -- '--memory-limit=4G' "$CALL_LOG")" -eq 2 ] \
    || fail "full/no-report: default memory limit should apply to both the run and the retry"

echo "PASS: PHPStan retry hands the full report to the summary and findings sidecar"
