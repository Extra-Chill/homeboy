#!/usr/bin/env bash
# Replay of Extra-Chill/homeboy#18516: a failing full-suite WordPress run that
# routes standalone PHP smokes through host PHP and PHPUnit through WP Codebox
# must deliver every failure to core through HOMEBOY_TEST_FAILURES_FILE.
#
# Route: test-runner.sh (full suite) -> standalone PHP smokes (failure records
# appended by the sidecar writer) -> test-runner-wp-codebox.sh ->
# wp-codebox-phpunit-adapter.mjs -> parse-test-failures.sh on the published
# artifact. The fake WP Codebox CLI reproduces the artifact shape of
# Extra-Chill/extrachill run 44062955343 (extrachill-users): 17 PHPUnit
# failures in test-results.json and the PHPUnit failure sections in the
# referenced commands log.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
FIXTURE="${EXTENSION_PATH}/tests/fixtures/wp-codebox-full-suite-failures"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

component="${work}/extrachill-users"
mkdir -p "${component}/tests/unit" "${component}/tests/integration"
printf '<?php\n/* Plugin Name: Extra Chill Users */\n' > "${component}/extrachill-users.php"

# Component test files declaring the classes the PHPUnit output names. Two
# failures carry no component trace frame and resolve through their class.
while IFS=' ' read -r rel_path class_name; do
    printf '<?php\nclass %s extends WP_UnitTestCase {}\n' "$class_name" > "${component}/${rel_path}"
done <<'CLASSES'
tests/unit/ArtistMembershipAuthorizationTest.php Test_Artist_Membership_Authorization
tests/integration/AttendanceButtonDesignSystemTest.php Test_Attendance_Button_Design_System
tests/integration/OAuthConsentTemplateTest.php Test_OAuth_Consent_Template
tests/integration/PrivacyErasureTest.php Test_Users_Privacy_Erasure
tests/unit/ArtistInvitationRegistrationTest.php Test_Artist_Invitation_Registration
tests/unit/ConcertImportAttendanceValidationTest.php Test_Concert_Import_Attendance_Validation
tests/unit/ConcertTrackingAbilitiesTest.php Test_Concert_Tracking_Abilities
tests/unit/CoreSignupSurfaceTest.php Test_Core_Signup_Surface
tests/unit/EventCreatorTest.php Test_Event_Creator
tests/unit/NotificationUnsubscribeTokenTest.php Test_Notification_Unsubscribe_Token
tests/unit/OAuthServiceStandaloneTest.php Test_OAuth_Service_Standalone
tests/unit/PasswordResetRateLimitTest.php Test_Authentication_Rate_Limits
tests/unit/TeamMembershipRepositoryTest.php Test_Team_Membership_Repository
CLASSES

# Standalone PHP smokes selected by the convention default tests/**/*-smoke.php.
printf '<?php\necho "PASS: passing smoke\\n";\n' > "${component}/tests/passing-smoke.php"
printf '<?php\nfwrite(STDERR, "FAIL: standalone contract\\n");\nexit(3);\n' > "${component}/tests/failing-smoke.php"

# Homeboy seeds the declared failures sidecar with [] and its sidecar writer
# appends one record per emitted standalone failure.
cat > "${work}/prelude.sh" <<'SH'
homeboy_runner_init() {
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:?HOMEBOY_COMPONENT_PATH is required}"
    PLUGIN_PATH="$COMPONENT_PATH"
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:?HOMEBOY_EXTENSION_PATH is required}"
}
homeboy_sidecar_emit() {
    [ "$1" = test.failure ] || return 1
    jq -c --argjson item "$2" '. + [$item]' "$HOMEBOY_TEST_FAILURES_FILE" > "${HOMEBOY_TEST_FAILURES_FILE}.emit"
    mv "${HOMEBOY_TEST_FAILURES_FILE}.emit" "$HOMEBOY_TEST_FAILURES_FILE"
}
SH
cat > "${work}/write-test-results.sh" <<'SH'
homeboy_write_test_results() {
    jq -n --argjson total "$1" --argjson passed "$2" --argjson failed "$3" --argjson skipped "$4" \
        '{total:$total,passed:$passed,failed:$failed,skipped:$skipped}' > "$HOMEBOY_TEST_RESULTS_FILE"
}
SH

cat > "${work}/wp-codebox" <<'JS'
#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
const args = process.argv.slice(2);
const fixture = process.env.REPLAY_FIXTURE;
if (args.includes('--version')) { process.stdout.write('0.33.7'); process.exit(0); }
if (args.slice(-3).join(' ') === 'runtime descriptor --json') {
  process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } }));
  process.exit(0);
}
if (args[0] === 'recipe' && args[1] === 'build') {
  fs.writeFileSync(args[args.indexOf('--output') + 1], '{"schema":"wp-codebox/workspace-recipe/v1"}');
  process.exit(0);
}
if (args[0] !== 'recipe-run') { process.stderr.write(`unsupported fake wp-codebox command: ${args.join(' ')}\n`); process.exit(2); }
const artifacts = args[args.indexOf('--artifacts') + 1];
const runtime = path.join(artifacts, 'runtime-extrachill-users');
fs.mkdirSync(path.join(runtime, 'files'), { recursive: true });
fs.mkdirSync(path.join(runtime, 'logs'), { recursive: true });
fs.writeFileSync(path.join(artifacts, 'latest-runtime.json'), JSON.stringify({ paths: { runtimeDirectory: 'runtime-extrachill-users' } }));
fs.copyFileSync(path.join(fixture, 'test-results.json'), path.join(runtime, 'files', 'test-results.json'));
const output = fs.readFileSync(path.join(fixture, 'phpunit-output.log'), 'utf8');
fs.writeFileSync(path.join(runtime, 'logs', 'commands.log'), output);
process.stdout.write(`${JSON.stringify({ schema: 'wp-codebox/recipe-run/v1', success: false, executions: [{ command: 'wordpress.phpunit', status: 'failed', exitCode: 2, stdout: output, stderr: '' }] })}\n`);
process.exitCode = 1;
JS
chmod +x "${work}/wp-codebox"

mkdir -p "${work}/invocation-artifacts"
printf '%s\n' '{"schema":"homeboy/artifact-manifest/v1"}' > "${work}/invocation-artifacts/homeboy-artifact-manifest.json"
printf '[]\n' > "${work}/test-failures.json"

set +e
REPLAY_FIXTURE="$FIXTURE" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="${work}/prelude.sh" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID=extrachill-users \
COMPONENT_ID=extrachill-users \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE=plugin \
HOMEBOY_SETTINGS_JSON='{}' \
HOMEBOY_WP_CODEBOX_BIN="${work}/wp-codebox" \
HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="${work}/artifacts" \
HOMEBOY_INVOCATION_ARTIFACT_DIR="${work}/invocation-artifacts" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${work}/write-test-results.sh" \
HOMEBOY_TEST_RESULTS_FILE="${work}/test-results.json" \
HOMEBOY_TEST_FAILURES_FILE="${work}/test-failures.json" \
    bash "${SCRIPT_DIR}/test-runner.sh" > "${work}/output" 2>&1
status=$?
set -e

fail() {
    echo "FAIL: $1" >&2
    sed -n '1,200p' "${work}/output" >&2
    [ ! -f "${work}/test-failures.json" ] || cat "${work}/test-failures.json" >&2
    exit 1
}

[ "$status" -eq 1 ] || fail "expected the failing PHPUnit backend status 1, got ${status}"
grep -Fq 'PHP_SMOKE_FAIL:tests/failing-smoke.php:exit=3' "${work}/output" || fail 'standalone smoke failure was not reported'
grep -Eq '^FULL_SUITE_STANDALONE_PHP_SUMMARY:.* selected=2 routed=2 .*passed=1 failed=1$' "${work}/output" \
    || fail 'standalone smoke summary missing'
grep -Fq 'Running PHPUnit tests via WP Codebox...' "${work}/output" || fail 'WP Codebox PHPUnit route did not run'
jq -e '.total == 641 and .passed == 617 and .failed == 17 and .skipped == 7' "${work}/test-results.json" >/dev/null \
    || fail 'PHPUnit counts were not published to HOMEBOY_TEST_RESULTS_FILE'

# Core keeps the envelope's failures array (normalize_declared_test_failures),
# so every record must be in `.failures`, each bound to a component file.
python3 - "${work}/test-failures.json" "${work}/invocation-artifacts/wp-codebox-phpunit/files/test-failures.json" <<'PY' || fail 'failure sidecar did not carry every failure'
import json, sys

sidecar = json.load(open(sys.argv[1]))
failures = sidecar["failures"] if isinstance(sidecar, dict) else sidecar
by_id = {failure["test_id"]: failure for failure in failures}

assert len(failures) == 18, f"expected 1 standalone + 17 PHPUnit failures, got {len(failures)}"
assert len(by_id) == 18, "failure test IDs must be unique"

standalone = by_id["tests/failing-smoke.php"]
assert standalone["test_file"] == "tests/failing-smoke.php", standalone

phpunit = [failure for failure in failures if failure["test_id"] != "tests/failing-smoke.php"]
assert len(phpunit) == 17, len(phpunit)
assert sum(1 for failure in phpunit if failure.get("status") == "error") == 1
expected_files = {
    "Test_Artist_Membership_Authorization::test_revoked_and_foreign_memberships_cannot_manage_artist": "tests/unit/ArtistMembershipAuthorizationTest.php",
    "Test_OAuth_Consent_Template::test_consent_form_renders_the_nonce_action_for_each_grant": "tests/integration/OAuthConsentTemplateTest.php",
    "Test_Users_Privacy_Erasure::test_network_tables_are_erased_from_another_site": "tests/integration/PrivacyErasureTest.php",
    "Test_Notification_Unsubscribe_Token::test_route_schema_accepts_token_only": "tests/unit/NotificationUnsubscribeTokenTest.php",
    "Test_Authentication_Rate_Limits::test_authenticate_admits_fifth_failure_and_blocks_sixth": "tests/unit/PasswordResetRateLimitTest.php",
    "Test_Team_Membership_Repository::test_artist_corruption_errors_use_artist_codes_and_messages": "tests/unit/TeamMembershipRepositoryTest.php",
}
for test_id, test_file in expected_files.items():
    assert by_id[test_id]["test_file"] == test_file, (test_id, by_id[test_id]["test_file"])
for failure in phpunit:
    assert "::" in failure["test_id"], failure["test_id"]
    assert failure["test_file"].startswith("tests/") and not failure["test_file"].startswith("/"), failure
    assert isinstance(failure["fingerprint"], str) and len(failure["fingerprint"]) == 64, failure

# The published WP Codebox artifact keeps exactly the PHPUnit failures.
published = json.load(open(sys.argv[2]))
assert len(published["failures"]) == 17, len(published["failures"])
PY

echo 'WP Codebox full-suite failures replay smoke passed'
