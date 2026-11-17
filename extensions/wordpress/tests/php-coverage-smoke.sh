#!/usr/bin/env bash
set -euo pipefail
EXTENSION_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Checkout runs can use core's helpers; installed runs receive them from Homeboy.
if [ -z "${HOMEBOY_RUNTIME_SETTINGS_HELPER:-}" ] && [ -f "$EXTENSION_PATH/../../crates/homeboy-core/src/extension/runtime/settings.sh" ]; then
    export HOMEBOY_RUNTIME_SETTINGS_HELPER="$EXTENSION_PATH/../../crates/homeboy-core/src/extension/runtime/settings.sh"
fi
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
component="$work/component"
mkdir -p "$component/src" "$component/tests" "$work/artifacts"
cat > "$component/src/functions.php" <<'PHP'
<?php
function child_only() {
    $value = 21;
    return $value * 2;
}
function uncalled() {
    return 999;
}
PHP
cat > "$component/src/unloaded.php" <<'PHP'
<?php
class NeverLoaded {
    public function untouched() {
        return 123;
    }
}
PHP
cat > "$component/tests/child.php" <<'PHP'
<?php
require dirname(__DIR__) . '/src/functions.php';
exit(child_only() === 42 ? 0 : 1);
PHP
cat > "$component/tests/run.php" <<'PHP'
<?php
passthru('php ' . escapeshellarg(__DIR__ . '/child.php'), $status);
exit($status);
PHP
cat > "$work/prelude.sh" <<'SH'
homeboy_runner_init() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$COMPONENT_PATH"
}
SH
run_coverage() {
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$component" HOMEBOY_COMPONENT_ID=coverage-fixture \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$work/prelude.sh" \
    HOMEBOY_COMPONENT_SHAPE=plugin \
    HOMEBOY_SETTINGS_JSON='{"standalone_php_test_paths":["tests/run.php"]}' \
    HOMEBOY_COVERAGE=1 HOMEBOY_COVERAGE_FILE="$work/artifacts/summary.json" \
        bash "$EXTENSION_PATH/scripts/test/test-runner.sh"
}
run_coverage > "$work/run.log" 2>&1 || { read -r -d '' message < "$work/run.log" || true; echo "$message"; exit 1; }
python3 - "$work/artifacts" <<'PY'
import json, pathlib, sys, xml.etree.ElementTree as ET
root = pathlib.Path(sys.argv[1])
summary = json.loads((root / 'summary.json').read_text())
lines = summary['totals']['lines']
assert 0 < lines['covered'] < lines['total'], lines
assert 0 < lines['pct'] < 100, lines
assert isinstance(summary['totals']['methods']['pct'], (float, int))
assert all('file' in f and 'line_pct' in f for f in summary['files'])
methods = json.loads((root / 'coverage-methods.json').read_text())
assert next(m for m in methods if m['method'] == 'child_only')['executed_lines'] > 0
assert next(m for m in methods if m['method'] == 'uncalled')['executed_lines'] == 0
unloaded = next(m for m in methods if m['method'] == 'untouched')
assert unloaded['class'] == 'NeverLoaded' and unloaded['executed_lines'] == 0
assert unloaded['executable_lines'] > 0
xml = ET.parse(root / 'coverage.xml')
file = next(f for f in xml.findall('./project/file') if f.attrib['name'].endswith('/functions.php'))
assert file.find("line[@num='3']").attrib['count'] == '1'
assert file.find("line[@num='4']").attrib['count'] == '1'
PY
if HOMEBOY_COVERAGE_MIN=100 run_coverage > "$work/min.log" 2>&1; then
    echo 'FAIL: coverage minimum was not enforced' >&2
    exit 1
fi
grep -q 'COVERAGE FAILED' "$work/min.log"
if HOMEBOY_PCOV_PROBE_BIN=false run_coverage > "$work/missing.log" 2>&1; then
    echo 'FAIL: unloadable pcov succeeded' >&2
    exit 1
fi
grep -Eq 'install pcov for PHP [0-9]+\.[0-9]+ on runner' "$work/missing.log"
echo 'PASS: PHP coverage merges child processes, inventories dead methods, and gates coverage.'
