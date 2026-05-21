#!/usr/bin/env bash
#
# Regression smoke test for homeboy-extensions#458.
#
# Verifies that:
#   1. wp-filesystem-fixer.php SKIPS pure-PHP test harness files
#      (tests/*-smoke.php, tests/smoke-*.php, tests/*Test.php, tests/*TestCase.php).
#      Rewriting file_get_contents() to $wp_filesystem->get_contents() in a file
#      that doesn't bootstrap WordPress crashes at runtime.
#
#   2. short-ternary-fixer.php SKIPS expansion when the left expression contains
#      a function or method call. Naively duplicating `f($x) ?: 'd'` into
#      `f($x) ? f($x) : 'd'` double-invokes the call (extra I/O / side effects).
#
#   3. wp-filesystem-fixer.php still rewrites production runtime code.
#
#   4. short-ternary-fixer.php still expands variables, property chains, and
#      array access (which are safe to duplicate).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WP_FILESYSTEM_FIXER="${SCRIPT_DIR}/wp-filesystem-fixer.php"
SHORT_TERNARY_FIXER="${SCRIPT_DIR}/short-ternary-fixer.php"
LONELY_IF_FIXER="${SCRIPT_DIR}/lonely-if-fixer.php"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_grep() {
    local pattern="$1"
    local file="$2"
    local desc="$3"
    if ! grep -qE "$pattern" "$file"; then
        echo "--- file contents: $file ---" >&2
        cat "$file" >&2
        echo "---" >&2
        fail "$desc — expected pattern '$pattern' in $file"
    fi
}

assert_not_grep() {
    local pattern="$1"
    local file="$2"
    local desc="$3"
    if grep -qE "$pattern" "$file"; then
        echo "--- file contents: $file ---" >&2
        cat "$file" >&2
        echo "---" >&2
        fail "$desc — unexpected pattern '$pattern' in $file"
    fi
}

# === Bug 1: wp-filesystem-fixer must skip test harness files ===

mkdir -p "${TMPDIR}/tests" "${TMPDIR}/inc"

cat > "${TMPDIR}/tests/agent-bundle-smoke.php" <<'PHP'
<?php
$docs = file_get_contents( __DIR__ . '/fixture.md' );
PHP

cat > "${TMPDIR}/tests/SomethingTest.php" <<'PHP'
<?php
class SomethingTest {
    function setUp() {
        $x = file_get_contents( __DIR__ . '/fixture.txt' );
    }
}
PHP

cat > "${TMPDIR}/tests/SomethingTestCase.php" <<'PHP'
<?php
class SomethingTestCase {
    function it_reads() {
        $x = file_get_contents( __DIR__ . '/fixture.txt' );
    }
}
PHP

cat > "${TMPDIR}/inc/runtime.php" <<'PHP'
<?php
class Runtime {
    function read( $path ) {
        return file_get_contents( $path );
    }
}
PHP

php "$WP_FILESYSTEM_FIXER" "$TMPDIR" > /dev/null

assert_grep 'file_get_contents\( __DIR__' "${TMPDIR}/tests/agent-bundle-smoke.php" \
    "smoke harness must not be rewritten"
assert_not_grep 'wp_filesystem' "${TMPDIR}/tests/agent-bundle-smoke.php" \
    "smoke harness must not introduce wp_filesystem"

assert_grep 'file_get_contents\( __DIR__' "${TMPDIR}/tests/SomethingTest.php" \
    "PHPUnit *Test.php must not be rewritten"
assert_not_grep 'wp_filesystem' "${TMPDIR}/tests/SomethingTest.php" \
    "PHPUnit *Test.php must not introduce wp_filesystem"

assert_grep 'file_get_contents\( __DIR__' "${TMPDIR}/tests/SomethingTestCase.php" \
    "PHPUnit *TestCase.php must not be rewritten"

assert_grep 'wp_filesystem->get_contents' "${TMPDIR}/inc/runtime.php" \
    "production runtime code must still be rewritten"
assert_not_grep '\bfile_get_contents\b' "${TMPDIR}/inc/runtime.php" \
    "production runtime code must replace file_get_contents"

# === Bug 2: short-ternary-fixer must skip when left side contains a call ===

rm -rf "${TMPDIR}"/*
mkdir -p "${TMPDIR}"

cat > "${TMPDIR}/calls.php" <<'PHP'
<?php
$a = file_get_contents( $path ) ?: '';
$b = $obj->method( $x ) ?: 'fallback';
$c = some_func() ?: [];
PHP

cat > "${TMPDIR}/safe.php" <<'PHP'
<?php
$a = $b ?: 'default';
$c = $d->prop ?: 0;
$e = $f[$g] ?: [];
PHP

php "$SHORT_TERNARY_FIXER" "${TMPDIR}/calls.php" > /dev/null
php "$SHORT_TERNARY_FIXER" "${TMPDIR}/safe.php" > /dev/null

# Calls must remain as ?: (skipped by the fixer to avoid double-invocation).
assert_grep 'file_get_contents\( \$path \) \?: ' "${TMPDIR}/calls.php" \
    "function call ?: must not be expanded (double-invoke regression)"
assert_grep 'method\( \$x \) \?: ' "${TMPDIR}/calls.php" \
    "method call ?: must not be expanded (double-invoke regression)"
assert_grep 'some_func\(\) \?: ' "${TMPDIR}/calls.php" \
    "bare function call ?: must not be expanded"

# Variables, properties, and array access are safe to duplicate.
assert_grep '\$b \? \$b :' "${TMPDIR}/safe.php" \
    "variable ?: must still be expanded"
assert_grep '\$d->prop \? \$d->prop :' "${TMPDIR}/safe.php" \
    "property access ?: must still be expanded"
assert_grep '\$f\[\$g\] \? \$f\[\$g\] :' "${TMPDIR}/safe.php" \
    "array access ?: must still be expanded"

# === End-to-end: chained smoke test (matches the data-machine repro) ===

mkdir -p "${TMPDIR}/repro/tests"
cat > "${TMPDIR}/repro/tests/agent-bundle-installed-artifact-smoke.php" <<'PHP'
<?php
$docs = file_get_contents( dirname( __DIR__ ) . '/docs/agent-bundles.md' ) ?: '';
echo strlen( $docs );
PHP

# Run both fixers in the same order as lint-runner.sh (short-ternary then wp-filesystem).
php "$SHORT_TERNARY_FIXER" "${TMPDIR}/repro/tests/agent-bundle-installed-artifact-smoke.php" > /dev/null
php "$WP_FILESYSTEM_FIXER" "${TMPDIR}/repro/tests/agent-bundle-installed-artifact-smoke.php" > /dev/null

# After both fixers, the original file must be untouched (test harness skip).
assert_grep "file_get_contents\( dirname\( __DIR__ \) \. '/docs/agent-bundles.md' \) \?: ''" \
    "${TMPDIR}/repro/tests/agent-bundle-installed-artifact-smoke.php" \
    "data-machine repro: smoke file must be untouched after both fixers"

# Sanity check: the file is still valid PHP.
php -l "${TMPDIR}/repro/tests/agent-bundle-installed-artifact-smoke.php" > /dev/null

# === Bug 3: PHP fixers must skip single non-PHP file paths ===

rm -rf "${TMPDIR}"/*
mkdir -p "${TMPDIR}/assets"

cat > "${TMPDIR}/assets/HandlerModel.js" <<'JS'
export default function normalize( config, normalized, key ) {
	if ( config.type === 'object' ) {
		normalized[ key ] = {};
	} else {
		if ( config.type === 'checkbox' ) {
			normalized[ key ] = !! normalized[ key ];
		}
	}
}
JS

php "$LONELY_IF_FIXER" "${TMPDIR}/assets/HandlerModel.js" > /dev/null

assert_grep '} else {' "${TMPDIR}/assets/HandlerModel.js" \
	"single-file PHP fixer input must skip JavaScript files"
assert_grep 'if \( config\.type === '\''checkbox'\'' \)' "${TMPDIR}/assets/HandlerModel.js" \
	"JavaScript nested if must not become PHP elseif"
assert_not_grep 'elseif' "${TMPDIR}/assets/HandlerModel.js" \
	"JavaScript file must not contain PHP elseif after lonely-if fixer"

echo "OK: fixer test harness skip + short-ternary call guard regression smoke passed"
