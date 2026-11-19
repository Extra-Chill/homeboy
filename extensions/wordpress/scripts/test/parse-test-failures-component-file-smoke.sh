#!/usr/bin/env bash
# Failures must name the component-relative test file the inventory plans, so
# Homeboy can bind each `Class::method` failure to its planned file unit
# (Extra-Chill/homeboy#18425). Sandboxed runtimes report mount paths, and
# assertions raised inside the WordPress test library carry no test frame.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-component-file.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

COMPONENT="$TMP_DIR/my-plugin"
mkdir -p "$COMPONENT/tests/unit" "$COMPONENT/tests/integration"
printf '<?php\nclass Test_Foo extends WP_UnitTestCase {}\n' > "$COMPONENT/tests/unit/FooTest.php"
printf '<?php\nfinal class Test_Consent_Template extends WP_UnitTestCase {}\n' > "$COMPONENT/tests/integration/ConsentTemplateTest.php"

PHPUNIT_OUTPUT="$TMP_DIR/phpunit-output.txt"
FAILURES_FILE="$TMP_DIR/failures.json"
cat > "$PHPUNIT_OUTPUT" <<'EOF'
There were 3 failures:

1) Test_Foo::test_a
Failed asserting that false is true.

/wordpress/wp-content/plugins/my-plugin/tests/unit/FooTest.php:12
/internal/eval.php:1588

2) Test_Foo::test_b
Failed asserting that false is true.

/wordpress/wp-content/plugins/my-plugin/tests/unit/FooTest.php:20
/internal/eval.php:1588

3) Test_Consent_Template::test_renders
Failed to assert that Theme without header.php triggered a deprecation notice.
Failed asserting that an array is empty.

/tmp/wp-codebox-inputs/vendor/wp-phpunit/wp-phpunit/includes/abstract-testcase.php:660
/internal/eval.php:1588

FAILURES!
Tests: 5, Assertions: 9, Failures: 3.
EOF

HOMEBOY_TEST_FAILURES_FILE="$FAILURES_FILE" \
    bash "$SCRIPT_DIR/parse-test-failures.sh" "$PHPUNIT_OUTPUT" "$COMPONENT"

python3 - "$FAILURES_FILE" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    failures = json.load(handle)["failures"]

files = {failure["test_name"]: failure["test_file"] for failure in failures}
assert files == {
    "Test_Foo::test_a": "tests/unit/FooTest.php",
    "Test_Foo::test_b": "tests/unit/FooTest.php",
    "Test_Consent_Template::test_renders": "tests/integration/ConsentTemplateTest.php",
}, files
PY

echo "wordpress parse-test-failures component test-file smoke passed"
