#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DETECTOR="${EXTENSION_DIR}/scripts/audit/wp-test-smells.py"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

mkdir -p "${TMPDIR}/tests/phpunit/tests/date" "${TMPDIR}/tests/phpunit/tests/query"

cat > "${TMPDIR}/tests/phpunit/tests/date/getFeedBuildDate.php" <<'PHP'
<?php
class Tests_Date_Get_Feed_Build_Date extends WP_UnitTestCase {
    public function test_feed_build_date_uses_latest_post() {
        $id = self::factory()->post->create( array( 'post_date' => '2026-04-26 10:00:00' ) );
        $wp_query = new WP_Query();
        $wp_query->posts = array( $id );
        $wp_query->post_count = 1;
    }
}
PHP

cat > "${TMPDIR}/tests/phpunit/tests/query/realFixture.php" <<'PHP'
<?php
class Tests_Query_Real_Fixture extends WP_UnitTestCase {
    public function test_query_posts_fixture() {
        $id = self::factory()->post->create();
        query_posts( array( 'posts__in' => array( $id ), 'fields' => 'ids' ) );
        $this->assertSame( array( $id ), $GLOBALS['wp_query']->posts );
    }

    public function test_real_query_args_are_ok() {
        $query = new WP_Query( array( 'post_type' => 'post' ) );
        $this->assertIsArray( $query->posts );
    }
}
PHP

set +e
json_output="$(python3 "$DETECTOR" --json "$TMPDIR")"
status=$?
set -e

if [ "$status" -ne 1 ]; then
    echo "Expected detector to exit 1 for mock-over-fixture fixture, got $status" >&2
    exit 1
fi

printf '%s' "$json_output" | python3 -c '
import json, sys
data = json.load(sys.stdin)
findings = data["findings"]
assert len(findings) == 1, findings
finding = findings[0]
assert finding["code"] == "wp.test.mock_over_fixture", finding
assert finding["file"] == "tests/phpunit/tests/date/getFeedBuildDate.php", finding
assert finding["method"] == "test_feed_build_date_uses_latest_post", finding
assert finding["variable"] == "wp_query", finding
assert finding["fields"] == ["posts", "post_count"], finding
assert "self::factory()->post->create" in finding["message"], finding
assert "query_posts" in finding["message"], finding
'

rm "${TMPDIR}/tests/phpunit/tests/date/getFeedBuildDate.php"

python3 "$DETECTOR" --json "$TMPDIR" | python3 -c '
import json, sys
data = json.load(sys.stdin)
assert data["findings"] == [], data
'

cat > "${TMPDIR}/tests/phpunit/tests/query/suppressed.php" <<'PHP'
<?php
class Tests_Query_Suppressed extends WP_UnitTestCase {
    public function test_intentional_impossible_query_state() {
        // homeboy-ignore wp.test.mock_over_fixture
        $query = new WP_Query();
        $query->posts = array( 123 );
        $query->found_posts = 1;
    }
}
PHP

python3 "$DETECTOR" --json "$TMPDIR" | python3 -c '
import json, sys
data = json.load(sys.stdin)
assert data["findings"] == [], data
'

echo "wp-test-smells smoke passed"
