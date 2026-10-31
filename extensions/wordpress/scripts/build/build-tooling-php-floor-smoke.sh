#!/usr/bin/env bash
set -euo pipefail

# Covers issue #2858: a component declaring a `Requires PHP` floor below
# build.sh's own tooling floor (8.0, for str_starts_with/str_contains in the
# JSON/glob heredocs) must not produce a bare "Standard input code" fatal.
#
# Simulates the release environment picking an ambient `php` that matches a
# component's low declared floor by putting a fake `php` (reports major
# version 7) first on PATH. Two scenarios:
#
#   1. No PHP 8.0+ interpreter anywhere on PATH: build.sh must fail fast with
#      an actionable message, not the undefined-function fatal.
#   2. A versioned php8.3 binary is on PATH alongside the low-floor `php`:
#      build.sh must route its own tooling heredocs through php8.3 (so the
#      old php's heredoc branch is never invoked) while still running
#      validate_php_syntax()'s component lint (`php -l`) through the ambient
#      low-floor `php`, preserving floor parity for the component's own code.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RESOLVE_CONTEXT_CORE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-tooling-php-floor.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

REAL_PHP_BIN="$(command -v php)"

make_component() {
    local dir="$1"
    mkdir -p "$dir"
    cat > "${dir}/low-floor-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Low Floor Plugin
 * Version: 1.0.0
 * Requires PHP: 7.4
 */
PHP
}

# Fake `php` (and every versioned candidate name resolve_tooling_php_bin()
# checks) that reports PHP_MAJOR_VERSION 7, mimicking an ambient interpreter
# pinned to a component's declared 7.4 floor with no newer interpreter
# installed anywhere else on the box. Delegates `-l` to the real interpreter
# so validate_php_syntax() still passes, and fatals like the real PHP 7.4
# would if a heredoc using str_starts_with reaches it. Shadowing every
# candidate name (not just `php`) is deliberate: it isolates the test from
# whatever PHP versions happen to already be installed on the host running
# this smoke test.
write_low_floor_php_stub() {
    local bin_dir="$1"
    local name
    for name in php php8.4 php8.3 php8.2 php8.1 php8.0; do
        cat > "${bin_dir}/${name}" <<SH
#!/usr/bin/env bash
if [ "\${1:-}" = "-r" ]; then
    echo 7
    exit 0
fi
if [ "\${1:-}" = "-l" ]; then
    printf 'lint\n' >> "${TMP_DIR}/lint-marker"
    exec "${REAL_PHP_BIN}" "\$@"
fi
printf 'heredoc\n' >> "${TMP_DIR}/heredoc-marker"
echo "PHP Fatal error: Uncaught Error: Call to undefined function str_starts_with() in Standard input code:1" >&2
exit 255
SH
        chmod +x "${bin_dir}/${name}"
    done
}

write_modern_php_wrapper() {
    local bin_dir="$1"
    local name="$2"
    cat > "${bin_dir}/${name}" <<SH
#!/usr/bin/env bash
exec "${REAL_PHP_BIN}" "\$@"
SH
    chmod +x "${bin_dir}/${name}"
}

# --- Scenario 1: no PHP 8.0+ interpreter anywhere on PATH ------------------

scenario1_dir="${TMP_DIR}/no-modern-php"
scenario1_bin="${TMP_DIR}/bin-no-modern-php"
mkdir -p "$scenario1_bin"
make_component "$scenario1_dir"
write_low_floor_php_stub "$scenario1_bin"

scenario1_out="${TMP_DIR}/scenario1.out"
if (
    cd "$scenario1_dir"
    PATH="${scenario1_bin}:$PATH" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="low-floor-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "$scenario1_out" 2>&1
); then
    echo "Expected build to fail with no PHP 8.0+ interpreter available" >&2
    sed 's/^/  /' "$scenario1_out" >&2
    exit 1
fi

if ! grep -Fq "WordPress packaging build tooling requires PHP 8.0 or newer." "$scenario1_out"; then
    echo "Expected actionable PHP floor preflight message" >&2
    sed 's/^/  /' "$scenario1_out" >&2
    exit 1
fi

if ! grep -Fq "independent of" "$scenario1_out"; then
    echo "Expected preflight message to distinguish tooling floor from component floor" >&2
    sed 's/^/  /' "$scenario1_out" >&2
    exit 1
fi

if grep -Fq "Standard input code" "$scenario1_out"; then
    echo "Expected the actionable preflight message, not the raw undefined-function fatal" >&2
    sed 's/^/  /' "$scenario1_out" >&2
    exit 1
fi

if [ -f "${TMP_DIR}/heredoc-marker" ]; then
    echo "Expected preflight to fail before any tooling heredoc ran" >&2
    exit 1
fi

# --- Scenario 2: low-floor php plus a versioned php8.3 on PATH -------------

scenario2_dir="${TMP_DIR}/modern-php-available"
scenario2_bin="${TMP_DIR}/bin-modern-php-available"
mkdir -p "$scenario2_bin"
make_component "$scenario2_dir"
write_low_floor_php_stub "$scenario2_bin"
write_modern_php_wrapper "$scenario2_bin" "php8.3"

scenario2_out="${TMP_DIR}/scenario2.out"
(
    cd "$scenario2_dir"
    PATH="${scenario2_bin}:$PATH" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="low-floor-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "$scenario2_out" 2>&1
)

if [ ! -f "${scenario2_dir}/build/low-floor-plugin.zip" ]; then
    echo "Expected build to succeed and produce a ZIP when php8.3 is available" >&2
    sed 's/^/  /' "$scenario2_out" >&2
    exit 1
fi

if [ -f "${TMP_DIR}/heredoc-marker" ]; then
    echo "Expected tooling heredocs to run under php8.3, never the low-floor php" >&2
    sed 's/^/  /' "$scenario2_out" >&2
    exit 1
fi

if [ ! -f "${TMP_DIR}/lint-marker" ]; then
    echo "Expected validate_php_syntax() to still lint under the ambient low-floor php" >&2
    sed 's/^/  /' "$scenario2_out" >&2
    exit 1
fi

echo "WordPress build tooling PHP floor smoke passed."
