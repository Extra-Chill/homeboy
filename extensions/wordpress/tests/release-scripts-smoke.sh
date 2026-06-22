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
#   - scripts/release/publish.sh skips the release-latest branch mirror when
#     homeboy.json does not configure it and emits a null branch in the
#     receipt.
#   - scripts/release/publish.sh executes the release-latest branch mirror
#     when homeboy.json configures it and emits the branch name in the
#     receipt.
#
# Stubs gh and git for the happy-path tests so we never touch the network
# or a real GitHub repository.

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
STUB_BIN_DIR="$(mktemp -d -t homeboy-wp-release-bin.XXXXXX)"
trap 'rm -rf "${WORK_DIR}" "${STUB_BIN_DIR}"' EXIT
cd "${WORK_DIR}"

failures=0

# Build a tiny stub for gh that always succeeds for "release upload" and a
# pass-through stub for git. The real binaries stay on PATH for everything
# else.
cat > "${STUB_BIN_DIR}/gh" <<'STUB_GH'
#!/usr/bin/env bash
# gh stub for release-scripts-smoke. Accept any args; just succeed.
case "$1" in
  release)
    shift
    case "$1" in
      upload)
        # gh release upload <tag> <file> --clobber --repo <slug>
        echo "stub: gh release upload $*" >&2
        exit 0
        ;;
    esac
    ;;
esac
echo "stub: gh $*" >&2
exit 0
STUB_GH
chmod +x "${STUB_BIN_DIR}/gh"

# ---------------------------------------------------------------------------
# package.sh: emits artifact JSON and forwards component WordPress settings
# when build/<slug>.zip exists.
# ---------------------------------------------------------------------------
mkdir -p build
echo "fake-zip-bytes" > build/test-plugin.zip
cat > homeboy.json <<'JSON'
{
  "id": "test-plugin",
  "extensions": {
    "wordpress": {
      "settings": {
        "build_nested_packages": false,
        "package_artifacts": ["runtime/packages/*.zip"]
      }
    }
  }
}
JSON

# Replace the real build script with a no-op so we don't actually run the
# WordPress build harness (which needs composer, node, plugin headers, …).
PACKAGE_SCRIPT_DIR="$(mktemp -d -t homeboy-wp-release-pkg.XXXXXX)"
trap 'rm -rf "${WORK_DIR}" "${PACKAGE_SCRIPT_DIR}"' EXIT
mkdir -p "${PACKAGE_SCRIPT_DIR}/scripts/build" "${PACKAGE_SCRIPT_DIR}/scripts/release"
cat > "${PACKAGE_SCRIPT_DIR}/scripts/build/build.sh" <<'STUB'
#!/usr/bin/env bash
if ! printf '%s' "${HOMEBOY_SETTINGS_JSON:-}" | jq -e '.build_nested_packages == false' >/dev/null; then
  echo "stub build did not receive build_nested_packages=false: ${HOMEBOY_SETTINGS_JSON:-}" >&2
  exit 1
fi
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

set +e
invalid_settings_output="$(HOMEBOY_COMPONENT_ID=test-plugin HOMEBOY_SETTINGS_JSON='not json' "${PACKAGE_SCRIPT_DIR}/scripts/release/package.sh" 2>/dev/null)"
invalid_settings_status=$?
set -e

if [[ ${invalid_settings_status} -ne 0 ]]; then
  echo "FAIL: package.sh did not fall back safely for invalid HOMEBOY_SETTINGS_JSON" >&2
  failures=$((failures + 1))
elif ! echo "${invalid_settings_output}" | jq -e '.[0].path == "build/test-plugin.zip"' >/dev/null 2>&1; then
  echo "FAIL: package.sh invalid-settings fallback produced unexpected JSON; got: ${invalid_settings_output}" >&2
  failures=$((failures + 1))
else
  echo "OK: package.sh falls back safely for invalid HOMEBOY_SETTINGS_JSON"
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

# ---------------------------------------------------------------------------
# publish.sh: skips release-latest branch when homeboy.json does not declare
# extensions.wordpress.release_latest_branch. Receipt has release_latest_branch=null.
# ---------------------------------------------------------------------------
HAPPY_DIR="$(mktemp -d -t homeboy-wp-release-happy.XXXXXX)"
trap 'rm -rf "${WORK_DIR}" "${STUB_BIN_DIR}" "${HAPPY_DIR}"' EXIT
mkdir -p "${HAPPY_DIR}/build"

# Produce a minimal valid ZIP that unzip can read in the configured-branch
# test below. Python ships in CI and on every supported workstation.
python3 -c "
import zipfile
with zipfile.ZipFile('${HAPPY_DIR}/build/happy-plugin.zip', 'w') as z:
  z.writestr('happy-plugin/happy-plugin.php', '<?php\n/**\n * Plugin Name: Happy Plugin\n * Version: 1.0.0\n */')
  z.writestr('happy-plugin/readme.txt', 'happy plugin readme')
"

# No homeboy.json — release_latest_branch should be empty and skipped.
set +e
publish_out="$(
  cd "${HAPPY_DIR}" && \
  PATH="${STUB_BIN_DIR}:${PATH}" \
  GITHUB_REPOSITORY="example/happy-plugin" \
  HOMEBOY_SETTINGS_JSON='{"release":{"tag":"v1.0.0","component_id":"happy-plugin"}}' \
  "${PUBLISH_SH}" 2>&1
)"
publish_status=$?
set -e

if [[ ${publish_status} -ne 0 ]]; then
  echo "FAIL: publish.sh (no branch) exited ${publish_status}; output: ${publish_out}" >&2
  failures=$((failures + 1))
else
  receipt="$(echo "${publish_out}" | tail -1)"
  if ! echo "${receipt}" | jq -e '.success == true and .release_latest_branch == null' >/dev/null 2>&1; then
    echo "FAIL: publish.sh receipt (no branch) missing success/null branch; got: ${receipt}" >&2
    failures=$((failures + 1))
  else
    echo "OK: publish.sh skips branch mirror when release_latest_branch is unset"
  fi
fi

# ---------------------------------------------------------------------------
# publish.sh: pushes release-latest branch when homeboy.json declares it.
# Stub git so we record the push without touching a real remote.
# ---------------------------------------------------------------------------
BRANCH_DIR="$(mktemp -d -t homeboy-wp-release-branch.XXXXXX)"
trap 'rm -rf "${WORK_DIR}" "${STUB_BIN_DIR}" "${HAPPY_DIR}" "${BRANCH_DIR}"' EXIT
mkdir -p "${BRANCH_DIR}/build"
cat > "${BRANCH_DIR}/homeboy.json" <<'JSON'
{
  "id": "happy-plugin",
  "extensions": {
    "wordpress": {
      "release_latest_branch": "release-latest"
    }
  }
}
JSON
python3 -c "
import zipfile
with zipfile.ZipFile('${BRANCH_DIR}/build/happy-plugin.zip', 'w') as z:
  z.writestr('happy-plugin/happy-plugin.php', '<?php\n/**\n * Plugin Name: Happy Plugin\n * Version: 1.0.0\n */')
  z.writestr('happy-plugin/readme.txt', 'happy plugin readme')
"

# Wrap git so push --force is intercepted (no real remote available).
PUSH_LOG="${BRANCH_DIR}/git-push.log"
cat > "${STUB_BIN_DIR}/git" <<STUB_GIT
#!/usr/bin/env bash
# Stub git: forward most subcommands to the real git but intercept "push"
# (anywhere in the argument list, since publish.sh uses 'git -C <path>
# push ...'). Log the push invocation for assertion.
for arg in "\$@"; do
  if [[ "\$arg" == "push" ]]; then
    printf '%s\n' "\$*" >> "${PUSH_LOG}"
    exit 0
  fi
done
exec /usr/bin/env PATH="\${HOMEBOY_REAL_PATH}" git "\$@"
STUB_GIT
chmod +x "${STUB_BIN_DIR}/git"

set +e
REAL_PATH="${PATH}"
publish_out="$(
  cd "${BRANCH_DIR}" && \
  HOMEBOY_REAL_PATH="${REAL_PATH}" \
  PATH="${STUB_BIN_DIR}:${REAL_PATH}" \
  GITHUB_REPOSITORY="example/happy-plugin" \
  GH_TOKEN="stub-token" \
  HOMEBOY_SETTINGS_JSON='{"release":{"tag":"v1.0.0","component_id":"happy-plugin"}}' \
  "${PUBLISH_SH}" 2>&1
)"
publish_status=$?
set -e

if [[ ${publish_status} -ne 0 ]]; then
  echo "FAIL: publish.sh (with branch) exited ${publish_status}; output: ${publish_out}" >&2
  failures=$((failures + 1))
else
  receipt="$(echo "${publish_out}" | tail -1)"
  if ! echo "${receipt}" | jq -e '.release_latest_branch == "release-latest"' >/dev/null 2>&1; then
    echo "FAIL: publish.sh receipt (with branch) missing release-latest; got: ${receipt}" >&2
    failures=$((failures + 1))
  elif [[ ! -s "${PUSH_LOG}" ]]; then
    echo "FAIL: publish.sh did not invoke git push for the configured branch" >&2
    failures=$((failures + 1))
  elif ! grep -q "release-latest:release-latest" "${PUSH_LOG}"; then
    echo "FAIL: publish.sh did not force-push release-latest:release-latest; got: $(cat "${PUSH_LOG}")" >&2
    failures=$((failures + 1))
  else
    echo "OK: publish.sh force-pushes release-latest branch when configured"
  fi
fi

if [[ ${failures} -gt 0 ]]; then
  echo "FAIL: release-scripts-smoke had ${failures} failure(s)" >&2
  exit 1
fi

echo "PASS: release-scripts-smoke"
