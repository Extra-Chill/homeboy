#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHPSTAN_RUNNER="${ROOT_DIR}/scripts/lint/phpstan-runner.sh"

assert() {
    local condition="$1"
    local message="$2"
    if ! eval "$condition"; then
        echo "FAIL: ${message}" >&2
        exit 1
    fi
}

# The guard must exist in the runner: probe function, --version probe, and an
# actionable skip message referencing the tracking issue.
grep -q "homeboy_phpstan_probe" "$PHPSTAN_RUNNER"
grep -q -- '--version' "$PHPSTAN_RUNNER"
grep -q '#2233' "$PHPSTAN_RUNNER"
grep -q "Reinstall the wordpress extension" "$PHPSTAN_RUNNER"

# Extract and exercise the probe function in isolation, the same way
# phpstan-temp-config-suffix-smoke.sh extracts homeboy_mktemp.
function_body=$(awk '/^homeboy_phpstan_probe\(\)/,/^}$/' "$PHPSTAN_RUNNER")
assert "[ -n \"\$function_body\" ]" "homeboy_phpstan_probe function should exist in runner"
eval "$function_body"

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

# Healthy binary (exit 0) → probe passes.
healthy_bin="${tmpdir}/phpstan-ok"
cat > "$healthy_bin" <<'SH'
#!/usr/bin/env bash
echo "PHPStan - PHP Static Analysis Tool 2.1.51"
exit 0
SH
chmod +x "$healthy_bin"
assert "homeboy_phpstan_probe \"\$healthy_bin\"" "probe should pass for a healthy phpstan binary"

# Corrupted binary (non-zero exit, simulating the PharException fatal) → probe fails.
broken_bin="${tmpdir}/phpstan-broken"
cat > "$broken_bin" <<'SH'
#!/usr/bin/env bash
echo "PharException: manifest cannot be larger than 100 MB in phar" >&2
exit 255
SH
chmod +x "$broken_bin"
if homeboy_phpstan_probe "$broken_bin"; then
    echo "FAIL: probe should fail for a corrupted phpstan binary" >&2
    exit 1
fi

echo "phpstan integrity guard smoke passed"
