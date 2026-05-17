#!/usr/bin/env bash
# Smoke test for the wordpress extension release pipeline scripts.
#
# Validates:
#   - scripts/release/package.sh emits a single-line JSON array with the
#     expected artifact shape when build/<slug>.zip exists.
#   - scripts/release/publish.sh refuses to run when HOMEBOY_SETTINGS_JSON is
#     missing.
#   - scripts/release/publish.sh refuses to run when the artifact ZIP is
#     missing.
#   - scripts/release/publish.sh refuses to run when release.tag is missing
#     from the payload.
#
# These checks exercise the failure paths without requiring gh, network, or
# a real GitHub repository — the happy path is exercised by the live release
# pipeline.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/.." && pwd)"
PACKAGE_SH="${EXTENSION_PATH}/scripts/release/package.sh"
PUBLISH_SH="${EXTENSION_PATH}/scripts/release/publish.sh"

if [[ ! -x "${PACKAGE_SH}" ]]; then
  echo "FAIL: package.sh is not executable" >&2
  exit 1
fi

if [[ ! -x "${PUBLISH_SH}" ]]; then
  echo "FAIL: publish.sh is not executable" >&2
  exit 1
fi

# Use a throwaway working directory so we don't disturb the extension repo.
WORK_DIR="$(mktemp -d -t homeboy-wp-release-smoke.XXXXXX)"
trap 'rm -rf "${WORK_DIR}"' EXIT
cd "${WORK_DIR}"

failures=0

# ---------------------------------------------------------------------------
# package.sh: emits artifact JSON when build/<slug>.zip exists.
# ---------------------------------------------------------------------------
mkdir -p build
echo "fake-zip-bytes" > build/test-plugin.zip

# Replace the real build script with a no-op so we don't actually run the
# WordPress build harness (which needs composer, node, plugin headers, …).
PACKAGE_SCRIPT_DIR="$(mktemp -d -t homeboy-wp-release-pkg.XXXXXX)"
trap 'rm -rf "${WORK_DIR}" "${PACKAGE_SCRIPT_DIR}"' EXIT
mkdir -p "${PACKAGE_SCRIPT_DIR}/scripts/build" "${PACKAGE_SCRIPT_DIR}/scripts/release"
cat > "${PACKAGE_SCRIPT_DIR}/scripts/build/build.sh" <<'STUB'
#!/usr/bin/env bash
echo "stub build" >&2
STUB
chmod +x "${PACKAGE_SCRIPT_DIR}/scripts/build/build.sh"
cp "${PACKAGE_SH}" "${PACKAGE_SCRIPT_DIR}/scripts/release/package.sh"
chmod +x "${PACKAGE_SCRIPT_DIR}/scripts/release/package.sh"

stub_output="$(HOMEBOY_COMPONENT_ID=test-plugin "${PACKAGE_SCRIPT_DIR}/scripts/release/package.sh" 2>/dev/null)" || stub_output=""

if [[ -z "${stub_output}" ]]; then
  echo "FAIL: package.sh produced no stdout" >&2
  failures=$((failures + 1))
elif ! echo "${stub_output}" | jq -e '.[0].path == "build/test-plugin.zip"' >/dev/null 2>&1; then
  echo "FAIL: package.sh JSON does not have expected path; got: ${stub_output}" >&2
  failures=$((failures + 1))
elif ! echo "${stub_output}" | jq -e '.[0].type == "wordpress-zip"' >/dev/null 2>&1; then
  echo "FAIL: package.sh JSON does not have type=wordpress-zip; got: ${stub_output}" >&2
  failures=$((failures + 1))
else
  echo "OK: package.sh emits expected artifact JSON"
fi

# ---------------------------------------------------------------------------
# publish.sh: missing HOMEBOY_SETTINGS_JSON should fail loudly.
# ---------------------------------------------------------------------------
set +e
unset HOMEBOY_SETTINGS_JSON
publish_err="$("${PUBLISH_SH}" 2>&1 >/dev/null)"
publish_status=$?
set -e

if [[ ${publish_status} -eq 0 ]]; then
  echo "FAIL: publish.sh exited 0 without HOMEBOY_SETTINGS_JSON" >&2
  failures=$((failures + 1))
elif ! echo "${publish_err}" | grep -q "HOMEBOY_SETTINGS_JSON is empty"; then
  echo "FAIL: publish.sh did not surface the missing-payload error; got: ${publish_err}" >&2
  failures=$((failures + 1))
else
  echo "OK: publish.sh rejects empty HOMEBOY_SETTINGS_JSON"
fi

# ---------------------------------------------------------------------------
# publish.sh: missing release.tag should fail loudly.
# ---------------------------------------------------------------------------
set +e
publish_err="$(HOMEBOY_SETTINGS_JSON='{"release":{}}' "${PUBLISH_SH}" 2>&1 >/dev/null)"
publish_status=$?
set -e

if [[ ${publish_status} -eq 0 ]]; then
  echo "FAIL: publish.sh exited 0 with missing release.tag" >&2
  failures=$((failures + 1))
elif ! echo "${publish_err}" | grep -q "release.tag"; then
  echo "FAIL: publish.sh did not surface the missing-tag error; got: ${publish_err}" >&2
  failures=$((failures + 1))
else
  echo "OK: publish.sh rejects missing release.tag"
fi

# ---------------------------------------------------------------------------
# publish.sh: missing artifact ZIP should fail loudly.
# ---------------------------------------------------------------------------
set +e
publish_err="$(HOMEBOY_SETTINGS_JSON='{"release":{"tag":"v0.0.0","component_id":"missing-plugin"}}' "${PUBLISH_SH}" 2>&1 >/dev/null)"
publish_status=$?
set -e

if [[ ${publish_status} -eq 0 ]]; then
  echo "FAIL: publish.sh exited 0 with missing artifact ZIP" >&2
  failures=$((failures + 1))
elif ! echo "${publish_err}" | grep -q "missing-plugin.zip"; then
  echo "FAIL: publish.sh did not surface the missing-artifact error; got: ${publish_err}" >&2
  failures=$((failures + 1))
else
  echo "OK: publish.sh rejects missing artifact ZIP"
fi

if [[ ${failures} -gt 0 ]]; then
  echo "FAIL: release-scripts-smoke had ${failures} failure(s)" >&2
  exit 1
fi

echo "PASS: release-scripts-smoke"
