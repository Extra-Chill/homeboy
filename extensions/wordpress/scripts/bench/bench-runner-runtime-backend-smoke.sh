#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-bench-backend-smoke.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT

assert_contains() {
    local file="$1"
    local needle="$2"
    if ! grep -qF "$needle" "$file"; then
        echo "Expected ${file} to contain: ${needle}" >&2
        exit 1
    fi
}

PREFLIGHT_HELPER="${TMPDIR}/bash-preflight.sh"
cat > "$PREFLIGHT_HELPER" <<'SH'
homeboy_require_bash_version() {
    return 0
}
SH

WP_CODEBOX_RUNNER="${TMPDIR}/wp-codebox-bench-runner.sh"
cat > "$WP_CODEBOX_RUNNER" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "WP_CODEBOX_BENCH_BACKEND:${*}"
SH
chmod +x "$WP_CODEBOX_RUNNER"

HOMEBOY_RUNTIME_BASH_PREFLIGHT="$PREFLIGHT_HELPER" \
HOMEBOY_RUNTIME_BENCH_RUNNER_WP_CODEBOX="$WP_CODEBOX_RUNNER" \
    bash "${SCRIPT_DIR}/bench-runner.sh" --fixture-arg > "${TMPDIR}/default.out"
assert_contains "${TMPDIR}/default.out" "WP_CODEBOX_BENCH_BACKEND:--fixture-arg"

HOMEBOY_RUNTIME_BASH_PREFLIGHT="$PREFLIGHT_HELPER" \
HOMEBOY_RUNTIME_BENCH_RUNNER_WP_CODEBOX="$WP_CODEBOX_RUNNER" \
HOMEBOY_WORDPRESS_BENCH_RUNTIME_BACKEND="wp-codebox" \
    bash "${SCRIPT_DIR}/bench-runner.sh" > "${TMPDIR}/explicit.out"
assert_contains "${TMPDIR}/explicit.out" "WP_CODEBOX_BENCH_BACKEND:"

set +e
HOMEBOY_RUNTIME_BASH_PREFLIGHT="$PREFLIGHT_HELPER" \
HOMEBOY_RUNTIME_BENCH_RUNNER_WP_CODEBOX="$WP_CODEBOX_RUNNER" \
HOMEBOY_WORDPRESS_BENCH_RUNTIME_BACKEND="core-native" \
    bash "${SCRIPT_DIR}/bench-runner.sh" > "${TMPDIR}/unsupported.out" 2>&1
unsupported_exit=$?
set -e

if [ "$unsupported_exit" -eq 0 ]; then
    echo "Expected unsupported backend to fail" >&2
    exit 1
fi
assert_contains "${TMPDIR}/unsupported.out" "unsupported WordPress bench runtime backend: core-native"
assert_contains "${TMPDIR}/unsupported.out" "Supported WordPress bench runtime backends: wp-codebox"

echo "bench runner runtime backend smoke passed"
