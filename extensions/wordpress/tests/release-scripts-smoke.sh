#!/usr/bin/env bash
# Smoke test for the wordpress extension release pipeline scripts.
#
# Validates:
#   - scripts/release/package.sh emits a single-line JSON array with the
#     expected artifact shape when build/<slug>.zip exists.
#   - scripts/release/package.sh builds declared additional_package_profiles
#     through build.sh with package_profile overridden per entry, emits the
#     primary ZIP followed by the additional assets in declared order, and
#     version-verifies every emitted ZIP.
#   - scripts/release/package.sh rejects malformed, unsafe, duplicate, and
#     primary-colliding additional package profile declarations before any
#     build runs.
#   - scripts/release/package.sh keeps emitting exactly the primary ZIP when
#     additional_package_profiles is not declared.
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
#   - scripts/release/publish.sh uses a single authoritative WordPress ZIP
#     recovery artifact and rejects ambiguous or invalid recovery artifacts.
#
# Stubs gh and git for the happy-path tests so we never touch the network
# or a real GitHub repository.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/.." && pwd)"
PACKAGE_SH="${EXTENSION_PATH}/scripts/release/package.sh"
PUBLISH_SH="${EXTENSION_PATH}/scripts/release/publish.sh"
UPDATE_DEP_SH="${EXTENSION_PATH}/scripts/release/update-dependency.sh"

if [[ ! -x "${PACKAGE_SH}" ]]; then
  echo "FAIL: package.sh is not executable" >&2
  exit 1
fi

if [[ ! -x "${PUBLISH_SH}" ]]; then
  echo "FAIL: publish.sh is not executable" >&2
  exit 1
fi

if [[ ! -x "${UPDATE_DEP_SH}" ]]; then
  echo "FAIL: update-dependency.sh is not executable" >&2
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
# package.sh: additional_package_profiles build through build.sh with
# package_profile overridden per entry and emit deterministic multi-artifact
# JSON (primary ZIP first, then declared additional assets in order).
# ---------------------------------------------------------------------------
PROFILE_SCRIPT_DIR="$(mktemp -d -t homeboy-wp-release-profiles.XXXXXX)"
trap 'rm -rf "${WORK_DIR}" "${STUB_BIN_DIR}" "${PACKAGE_SCRIPT_DIR}" "${PROFILE_SCRIPT_DIR}"' EXIT
mkdir -p "${PROFILE_SCRIPT_DIR}/scripts/build" "${PROFILE_SCRIPT_DIR}/scripts/release"
cat > "${PROFILE_SCRIPT_DIR}/scripts/build/build.sh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
if ! printf '%s' "${HOMEBOY_SETTINGS_JSON:-}" | jq -e '.build_nested_packages == false' >/dev/null; then
  echo "stub build did not receive build_nested_packages=false: ${HOMEBOY_SETTINGS_JSON:-}" >&2
  exit 1
fi
printf '%s\n' "$(printf '%s' "${HOMEBOY_SETTINGS_JSON:-}" | jq -c '.package_profile // null')" >> "${HOMEBOY_PROFILE_LOG}"
mkdir -p build
rm -f "build/${HOMEBOY_COMPONENT_ID}.zip"
python3 -c "
import zipfile
with zipfile.ZipFile('build/${HOMEBOY_COMPONENT_ID}.zip', 'w') as z:
  z.writestr('${HOMEBOY_COMPONENT_ID}/${HOMEBOY_COMPONENT_ID}.php', '<?php\n/**\n * Plugin Name: Profile Plugin\n * Version: 1.0.0\n */')
"
echo "stub build" >&2
STUB
chmod +x "${PROFILE_SCRIPT_DIR}/scripts/build/build.sh"
cp "${PACKAGE_SH}" "${PROFILE_SCRIPT_DIR}/scripts/release/package.sh"
chmod +x "${PROFILE_SCRIPT_DIR}/scripts/release/package.sh"
cp "${EXTENSION_PATH}/scripts/release/verify-artifact-version.sh" "${PROFILE_SCRIPT_DIR}/scripts/release/verify-artifact-version.sh"
chmod +x "${PROFILE_SCRIPT_DIR}/scripts/release/verify-artifact-version.sh"

PROFILE_DIR="$(mktemp -d -t homeboy-wp-release-profile-dir.XXXXXX)"
trap 'rm -rf "${WORK_DIR}" "${STUB_BIN_DIR}" "${PACKAGE_SCRIPT_DIR}" "${PROFILE_SCRIPT_DIR}" "${PROFILE_DIR}"' EXIT
cat > "${PROFILE_DIR}/homeboy.json" <<'JSON'
{
  "id": "profile-plugin",
  "extensions": {
    "wordpress": {
      "settings": {
        "build_nested_packages": false,
        "package_profile": { "manifest": "package-manifest.json", "profile": "full" },
        "additional_package_profiles": [
          { "manifest": "package-manifest.json", "profile": "runtime", "artifact": "profile-plugin-runtime.zip" },
          { "manifest": "package-manifest.json", "profile": "minimal", "artifact": "profile-plugin-minimal.zip" }
        ]
      }
    }
  }
}
JSON

PROFILE_PAYLOAD='{"release":{"version":"1.0.0","component_id":"profile-plugin"}}'
PROFILE_LOG="${PROFILE_DIR}/profile-invocations.log"

set +e
profile_out="$(
  cd "${PROFILE_DIR}" && \
  HOMEBOY_COMPONENT_ID=profile-plugin \
  HOMEBOY_SETTINGS_JSON="${PROFILE_PAYLOAD}" \
  HOMEBOY_PROFILE_LOG="${PROFILE_LOG}" \
  "${PROFILE_SCRIPT_DIR}/scripts/release/package.sh" 2>"${PROFILE_DIR}/package.err"
)"
profile_status=$?
set -e

if [[ ${profile_status} -ne 0 ]]; then
  echo "FAIL: package.sh (additional profiles) exited ${profile_status}; stderr: $(cat "${PROFILE_DIR}/package.err")" >&2
  failures=$((failures + 1))
elif ! echo "${profile_out}" | jq -e '
    length == 3
    and .[0].path == "build/profile-plugin.zip"
    and .[1].path == "build/profile-plugin-runtime.zip"
    and .[2].path == "build/profile-plugin-minimal.zip"
    and all(.[]; .type == "wordpress-zip" and .platform == null)
  ' >/dev/null 2>&1; then
  echo "FAIL: package.sh additional-profile JSON wrong; got: ${profile_out}" >&2
  failures=$((failures + 1))
elif [[ ! -f "${PROFILE_DIR}/build/profile-plugin-runtime.zip" || ! -f "${PROFILE_DIR}/build/profile-plugin-minimal.zip" || ! -f "${PROFILE_DIR}/build/profile-plugin.zip" ]]; then
  echo "FAIL: package.sh additional-profile run did not leave every artifact on disk" >&2
  failures=$((failures + 1))
elif [[ "$(wc -l < "${PROFILE_LOG}" | tr -d ' ')" != "3" ]] \
  || ! sed -n '1p' "${PROFILE_LOG}" | jq -e '.profile == "full"' >/dev/null 2>&1 \
  || ! sed -n '2p' "${PROFILE_LOG}" | jq -e '.profile == "runtime" and .manifest == "package-manifest.json"' >/dev/null 2>&1 \
  || ! sed -n '3p' "${PROFILE_LOG}" | jq -e '.profile == "minimal" and .manifest == "package-manifest.json"' >/dev/null 2>&1; then
  echo "FAIL: package.sh did not run build.sh once per profile with the right overrides; log: $(cat "${PROFILE_LOG}")" >&2
  failures=$((failures + 1))
elif ! grep -q "Verified build/profile-plugin.zip contains version 1.0.0" "${PROFILE_DIR}/package.err" \
  || ! grep -q "Verified build/profile-plugin-runtime.zip contains version 1.0.0" "${PROFILE_DIR}/package.err" \
  || ! grep -q "Verified build/profile-plugin-minimal.zip contains version 1.0.0" "${PROFILE_DIR}/package.err"; then
  echo "FAIL: package.sh did not version-verify every emitted ZIP; stderr: $(cat "${PROFILE_DIR}/package.err")" >&2
  failures=$((failures + 1))
else
  echo "OK: package.sh emits primary plus declared additional profile ZIPs in order"
fi

# ---------------------------------------------------------------------------
# package.sh: without additional_package_profiles the run stays exactly the
# primary ZIP and build.sh runs exactly once.
# ---------------------------------------------------------------------------
DEFAULT_DIR="$(mktemp -d -t homeboy-wp-release-default.XXXXXX)"
trap 'rm -rf "${WORK_DIR}" "${STUB_BIN_DIR}" "${PACKAGE_SCRIPT_DIR}" "${PROFILE_SCRIPT_DIR}" "${PROFILE_DIR}" "${DEFAULT_DIR}"' EXIT
cat > "${DEFAULT_DIR}/homeboy.json" <<'JSON'
{
  "id": "profile-plugin",
  "extensions": {
    "wordpress": {
      "settings": {
        "build_nested_packages": false,
        "package_profile": { "manifest": "package-manifest.json", "profile": "full" }
      }
    }
  }
}
JSON

DEFAULT_LOG="${DEFAULT_DIR}/profile-invocations.log"
set +e
default_out="$(
  cd "${DEFAULT_DIR}" && \
  HOMEBOY_COMPONENT_ID=profile-plugin \
  HOMEBOY_SETTINGS_JSON="${PROFILE_PAYLOAD}" \
  HOMEBOY_PROFILE_LOG="${DEFAULT_LOG}" \
  "${PROFILE_SCRIPT_DIR}/scripts/release/package.sh" 2>/dev/null
)"
default_status=$?
set -e

if [[ ${default_status} -ne 0 ]]; then
  echo "FAIL: package.sh (no additional profiles) exited ${default_status}" >&2
  failures=$((failures + 1))
elif ! echo "${default_out}" | jq -e 'length == 1 and .[0].path == "build/profile-plugin.zip" and .[0].type == "wordpress-zip"' >/dev/null 2>&1; then
  echo "FAIL: package.sh default JSON not exactly the primary ZIP; got: ${default_out}" >&2
  failures=$((failures + 1))
elif [[ "$(wc -l < "${DEFAULT_LOG}" | tr -d ' ')" != "1" ]]; then
  echo "FAIL: package.sh default run did not invoke build.sh exactly once; log: $(cat "${DEFAULT_LOG}")" >&2
  failures=$((failures + 1))
else
  echo "OK: package.sh without additional profiles emits exactly the primary ZIP"
fi

# ---------------------------------------------------------------------------
# package.sh: malformed, unsafe, duplicate, and primary-colliding
# additional_package_profiles declarations fail closed before any build runs.
# ---------------------------------------------------------------------------
rejection_cases="$(cat <<'CASES'
not-an-array|{"manifest":"m"}|must be an array of {manifest, profile, artifact} objects
entry-not-object|["nope"]|entries must be objects with manifest, profile, and artifact
missing-profile|[{"manifest":"m.json","artifact":"a.zip"}]|profile must be a non-empty string
non-string-manifest|[{"manifest":3,"profile":"p","artifact":"a.zip"}]|manifest must be a non-empty string
empty-artifact|[{"manifest":"m.json","profile":"p","artifact":""}]|artifact must be a non-empty string
path-like-artifact|[{"manifest":"m.json","profile":"p","artifact":"nested/evil.zip"}]|basename-only .zip names
traversal-artifact|[{"manifest":"m.json","profile":"p","artifact":"../evil.zip"}]|basename-only .zip names
non-zip-artifact|[{"manifest":"m.json","profile":"p","artifact":"evil.tar"}]|basename-only .zip names
hidden-artifact|[{"manifest":"m.json","profile":"p","artifact":".hidden.zip"}]|basename-only .zip names
primary-collision|[{"manifest":"m.json","profile":"p","artifact":"profile-plugin.zip"}]|cannot replace the primary release artifact
duplicate-artifact|[{"manifest":"m.json","profile":"p","artifact":"dup.zip"},{"manifest":"m.json","profile":"p2","artifact":"dup.zip"}]|artifact names must be unique
CASES
)"

while IFS='|' read -r case_name case_value case_grep; do
  [[ -n "${case_name}" ]] || continue

  REJECT_DIR="$(mktemp -d -t homeboy-wp-release-reject.XXXXXX)"
  REJECT_LOG="${REJECT_DIR}/invocations.log"
  jq -n --argjson value "${case_value}" \
    '{extensions:{wordpress:{settings:{build_nested_packages:false, additional_package_profiles:$value}}}}' \
    > "${REJECT_DIR}/homeboy.json"

  set +e
  reject_err="$(
    cd "${REJECT_DIR}" && \
    HOMEBOY_COMPONENT_ID=profile-plugin \
    HOMEBOY_SETTINGS_JSON="${PROFILE_PAYLOAD}" \
    HOMEBOY_PROFILE_LOG="${REJECT_LOG}" \
    "${PROFILE_SCRIPT_DIR}/scripts/release/package.sh" 2>&1 >/dev/null
  )"
  reject_status=$?
  set -e

  if [[ ${reject_status} -eq 0 ]]; then
    echo "FAIL: package.sh accepted additional profile declaration: ${case_name}" >&2
    failures=$((failures + 1))
  elif ! echo "${reject_err}" | grep -q "${case_grep}"; then
    echo "FAIL: ${case_name} did not surface the expected error; got: ${reject_err}" >&2
    failures=$((failures + 1))
  elif [[ -e "${REJECT_LOG}" ]]; then
    echo "FAIL: ${case_name} ran a build before validating declarations" >&2
    failures=$((failures + 1))
  else
    echo "OK: package.sh rejects ${case_name}"
  fi

  rm -rf "${REJECT_DIR}"
done <<< "${rejection_cases}"

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

# ---------------------------------------------------------------------------
# publish.sh: uses an untyped external recovery ZIP from Homeboy's directory
# inventory and returns its absolute path in the receipt.
# ---------------------------------------------------------------------------
RECOVERY_DIR="$(mktemp -d -t homeboy-wp-release-recovery.XXXXXX)"
trap 'rm -rf "${WORK_DIR}" "${STUB_BIN_DIR}" "${HAPPY_DIR}" "${BRANCH_DIR}" "${RECOVERY_DIR}"' EXIT
RECOVERY_ZIP="${RECOVERY_DIR}/recovered-plugin.zip"
python3 -c "
import zipfile
with zipfile.ZipFile('${RECOVERY_ZIP}', 'w') as z:
  z.writestr('recovered-plugin/recovered-plugin.php', '<?php\n/**\n * Plugin Name: Recovered Plugin\n * Version: 1.0.0\n */')
"

set +e
publish_out="$(
  cd "${RECOVERY_DIR}" && \
  PATH="${STUB_BIN_DIR}:${PATH}" \
  GITHUB_REPOSITORY="example/recovered-plugin" \
  HOMEBOY_SETTINGS_JSON='{"release":{"tag":"v1.0.0","component_id":"recovered-plugin","artifacts":[{"path":"'"${RECOVERY_ZIP}"'","phase":"recovery","producer":"from-artifacts","publication_authority":true}]}}' \
  "${PUBLISH_SH}" 2>&1
)"
publish_status=$?
set -e

if [[ ${publish_status} -ne 0 ]]; then
  echo "FAIL: publish.sh (recovery ZIP) exited ${publish_status}; output: ${publish_out}" >&2
  failures=$((failures + 1))
elif ! echo "${publish_out}" | tail -1 | jq -e --arg path "${RECOVERY_ZIP}" '.success == true and .artifact_path == $path' >/dev/null 2>&1; then
  echo "FAIL: publish.sh receipt did not return recovery ZIP path; got: ${publish_out}" >&2
  failures=$((failures + 1))
else
  echo "OK: publish.sh uses the untyped recovery ZIP"
fi

# publish.sh: multiple WordPress ZIP artifacts select the canonical component
# ZIP as primary, validate/upload every profile ZIP, and report them separately.
ADDITIONAL_RECOVERY_ZIP="${RECOVERY_DIR}/recovered-plugin-html-site-import.zip"
cp "${RECOVERY_ZIP}" "${ADDITIONAL_RECOVERY_ZIP}"
set +e
publish_out="$(
  cd "${RECOVERY_DIR}" && \
  PATH="${STUB_BIN_DIR}:${PATH}" \
  GITHUB_REPOSITORY="example/recovered-plugin" \
  HOMEBOY_SETTINGS_JSON='{"release":{"tag":"v1.0.0","component_id":"recovered-plugin","artifacts":[{"path":"'"${ADDITIONAL_RECOVERY_ZIP}"'","type":"wordpress-zip"},{"path":"'"${RECOVERY_ZIP}"'","artifact_type":"wordpress-zip"}]}}' \
  "${PUBLISH_SH}" 2>&1
)"
publish_status=$?
set -e

if [[ ${publish_status} -ne 0 ]]; then
  echo "FAIL: publish.sh rejected canonical primary plus additional ZIP: ${publish_out}" >&2
  failures=$((failures + 1))
elif ! echo "${publish_out}" | tail -1 | jq -e --arg primary "${RECOVERY_ZIP}" --arg additional "${ADDITIONAL_RECOVERY_ZIP}" '.artifact_path == $primary and .additional_artifact_paths == [$additional]' >/dev/null 2>&1; then
  echo "FAIL: publish.sh did not distinguish primary and additional ZIPs: ${publish_out}" >&2
  failures=$((failures + 1))
elif ! echo "${publish_out}" | grep -q "${ADDITIONAL_RECOVERY_ZIP} --clobber --repo example/recovered-plugin"; then
  echo "FAIL: publish.sh did not upload the additional ZIP: ${publish_out}" >&2
  failures=$((failures + 1))
else
  echo "OK: publish.sh uploads canonical primary plus additional profile ZIPs"
fi

# Multiple ZIPs without exactly one canonical component asset remain
# ambiguous and must fail closed.
set +e
publish_err="$(
  cd "${RECOVERY_DIR}" && \
  PATH="${STUB_BIN_DIR}:${PATH}" \
  GITHUB_REPOSITORY="example/recovered-plugin" \
  HOMEBOY_SETTINGS_JSON='{"release":{"tag":"v1.0.0","component_id":"recovered-plugin","artifacts":[{"path":"'"${ADDITIONAL_RECOVERY_ZIP}"'","type":"wordpress-zip"},{"path":"'"${ADDITIONAL_RECOVERY_ZIP}"'","artifact_type":"wordpress-zip"}]}}' \
  "${PUBLISH_SH}" 2>&1 >/dev/null
)"
publish_status=$?
set -e

if [[ ${publish_status} -eq 0 ]]; then
  echo "FAIL: publish.sh accepted ambiguous recovery ZIP artifacts" >&2
  failures=$((failures + 1))
elif ! echo "${publish_err}" | grep -q "require exactly one canonical recovered-plugin.zip primary artifact"; then
  echo "FAIL: publish.sh did not surface the ambiguous-recovery error; got: ${publish_err}" >&2
  failures=$((failures + 1))
else
  echo "OK: publish.sh rejects multiple ZIPs without one canonical primary"
fi

# publish.sh: a matching recovery artifact needs a non-empty string path.
set +e
publish_err="$(
  cd "${RECOVERY_DIR}" && \
  PATH="${STUB_BIN_DIR}:${PATH}" \
  GITHUB_REPOSITORY="example/recovered-plugin" \
  HOMEBOY_SETTINGS_JSON='{"release":{"tag":"v1.0.0","component_id":"recovered-plugin","artifacts":[{"path":null,"type":"wordpress-zip"}]}}' \
  "${PUBLISH_SH}" 2>&1 >/dev/null
)"
publish_status=$?
set -e

if [[ ${publish_status} -eq 0 ]]; then
  echo "FAIL: publish.sh accepted an invalid recovery ZIP path" >&2
  failures=$((failures + 1))
elif ! echo "${publish_err}" | grep -q "path must be a non-empty string"; then
  echo "FAIL: publish.sh did not surface the invalid-recovery-path error; got: ${publish_err}" >&2
  failures=$((failures + 1))
else
  echo "OK: publish.sh rejects invalid recovery ZIP paths"
fi

# publish.sh: a selected recovery artifact must resolve to a regular file.
set +e
publish_err="$(
  cd "${RECOVERY_DIR}" && \
  PATH="${STUB_BIN_DIR}:${PATH}" \
  GITHUB_REPOSITORY="example/recovered-plugin" \
  HOMEBOY_SETTINGS_JSON='{"release":{"tag":"v1.0.0","component_id":"recovered-plugin","artifacts":[{"path":"'"${RECOVERY_DIR}"'","type":"wordpress-zip"}]}}' \
  "${PUBLISH_SH}" 2>&1 >/dev/null
)"
publish_status=$?
set -e

if [[ ${publish_status} -eq 0 ]]; then
  echo "FAIL: publish.sh accepted a recovery ZIP path that is not a regular file" >&2
  failures=$((failures + 1))
elif ! echo "${publish_err}" | grep -q "expected release artifact"; then
  echo "FAIL: publish.sh did not reject the non-file recovery path; got: ${publish_err}" >&2
  failures=$((failures + 1))
else
  echo "OK: publish.sh rejects recovery ZIP paths that are not regular files"
fi

# ---------------------------------------------------------------------------
# update-dependency.sh: repins a Composer custom-package to a released upstream
# and mirrors the coordinates into composer.lock. Asserts the EXACT rewrite
# shape (version, dist.url archive tag, dist/source reference, require
# constraint). composer is skipped so the deterministic JSON rewrite is what is
# under test.
# ---------------------------------------------------------------------------
UPDATE_DIR="$(mktemp -d -t homeboy-wp-update-dep.XXXXXX)"
trap 'rm -rf "${WORK_DIR}" "${STUB_BIN_DIR}" "${HAPPY_DIR}" "${BRANCH_DIR}" "${UPDATE_DIR}"' EXIT

cat > "${UPDATE_DIR}/composer.json" <<'JSON'
{
  "name": "chubes/static-site-importer",
  "require": {
    "php": ">=8.1",
    "chubes/php-transformer": "1.3.0"
  },
  "repositories": [
    {
      "type": "package",
      "package": {
        "name": "chubes/php-transformer",
        "version": "1.3.0",
        "dist": {
          "type": "zip",
          "url": "https://github.com/chubes4/php-transformer/archive/refs/tags/v1.3.0.zip",
          "reference": "oldsha111"
        },
        "source": {
          "type": "git",
          "url": "https://github.com/chubes4/php-transformer.git",
          "reference": "oldsha111"
        }
      }
    }
  ]
}
JSON

cat > "${UPDATE_DIR}/composer.lock" <<'JSON'
{
  "content-hash": "stale000",
  "packages": [
    {
      "name": "chubes/php-transformer",
      "version": "1.3.0",
      "dist": {
        "type": "zip",
        "url": "https://github.com/chubes4/php-transformer/archive/refs/tags/v1.3.0.zip",
        "reference": "oldsha111"
      },
      "source": {
        "type": "git",
        "url": "https://github.com/chubes4/php-transformer.git",
        "reference": "oldsha111"
      }
    }
  ],
  "packages-dev": []
}
JSON

UPDATE_PAYLOAD='{"release":{"component_id":"static-site-importer","local_path":"'"${UPDATE_DIR}"'"},"dependency":{"released_id":"php-transformer","package":"chubes/php-transformer","version":"1.4.0","tag":"v1.4.0","sha":"newsha999"}}'

set +e
update_out="$(
  cd "${UPDATE_DIR}" && \
  HOMEBOY_SKIP_COMPOSER_UPDATE=1 \
  HOMEBOY_COMPONENT_ID="static-site-importer" \
  HOMEBOY_SETTINGS_JSON="${UPDATE_PAYLOAD}" \
  "${UPDATE_DEP_SH}" 2>/dev/null
)"
update_status=$?
set -e

if [[ ${update_status} -ne 0 ]]; then
  echo "FAIL: update-dependency.sh exited ${update_status}" >&2
  failures=$((failures + 1))
else
  cj="${UPDATE_DIR}/composer.json"
  expected_url="https://github.com/chubes4/php-transformer/archive/refs/tags/v1.4.0.zip"
  if ! jq -e --arg u "${expected_url}" '
        (.repositories[0].package.version == "1.4.0")
        and (.repositories[0].package.dist.url == $u)
        and (.repositories[0].package.dist.reference == "newsha999")
        and (.repositories[0].package.source.reference == "newsha999")
        and (.require["chubes/php-transformer"] == "1.4.0")
      ' "${cj}" >/dev/null 2>&1; then
    echo "FAIL: update-dependency.sh composer.json rewrite shape wrong; got: $(cat "${cj}")" >&2
    failures=$((failures + 1))
  elif ! echo "${update_out}" | jq -e '.success == true and .composer_lock_updated == true and .composer_refreshed == false' >/dev/null 2>&1; then
    echo "FAIL: update-dependency.sh receipt unexpected; got: ${update_out}" >&2
    failures=$((failures + 1))
  elif ! jq -e --arg u "${expected_url}" '
        (.packages[0].version == "1.4.0")
        and (.packages[0].dist.url == $u)
        and (.packages[0].dist.reference == "newsha999")
        and (.packages[0].source.reference == "newsha999")
      ' "${UPDATE_DIR}/composer.lock" >/dev/null 2>&1; then
    echo "FAIL: update-dependency.sh composer.lock mirror wrong; got: $(cat "${UPDATE_DIR}/composer.lock")" >&2
    failures=$((failures + 1))
  else
    echo "OK: update-dependency.sh repins custom-package version/dist/source/constraint + lock mirror"
  fi
fi

# update-dependency.sh: a package not declared as a custom-package repository
# must fail loudly rather than ship an unchanged pin.
NOPKG_DIR="$(mktemp -d -t homeboy-wp-update-nopkg.XXXXXX)"
trap 'rm -rf "${WORK_DIR}" "${STUB_BIN_DIR}" "${HAPPY_DIR}" "${BRANCH_DIR}" "${UPDATE_DIR}" "${NOPKG_DIR}"' EXIT
cat > "${NOPKG_DIR}/composer.json" <<'JSON'
{
  "name": "chubes/static-site-importer",
  "require": { "php": ">=8.1" },
  "repositories": []
}
JSON

set +e
nopkg_err="$(
  cd "${NOPKG_DIR}" && \
  HOMEBOY_SKIP_COMPOSER_UPDATE=1 \
  HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"chubes/php-transformer","version":"1.4.0","tag":"v1.4.0","sha":"x"}}' \
  "${UPDATE_DEP_SH}" 2>&1 >/dev/null
)"
nopkg_status=$?
set -e

if [[ ${nopkg_status} -eq 0 ]]; then
  echo "FAIL: update-dependency.sh exited 0 when package is not a custom-package repository" >&2
  failures=$((failures + 1))
elif ! echo "${nopkg_err}" | grep -q "custom-package repository"; then
  echo "FAIL: update-dependency.sh did not surface the missing custom-package error; got: ${nopkg_err}" >&2
  failures=$((failures + 1))
else
  echo "OK: update-dependency.sh fails loudly when package is not a custom-package repository"
fi

# update-dependency.sh: missing required dependency coordinates must fail.
set +e
missing_err="$(
  cd "${UPDATE_DIR}" && \
  HOMEBOY_SKIP_COMPOSER_UPDATE=1 \
  HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"chubes/php-transformer"}}' \
  "${UPDATE_DEP_SH}" 2>&1 >/dev/null
)"
missing_status=$?
set -e

if [[ ${missing_status} -eq 0 ]]; then
  echo "FAIL: update-dependency.sh exited 0 with missing version/tag" >&2
  failures=$((failures + 1))
else
  echo "OK: update-dependency.sh rejects missing dependency coordinates"
fi

if [[ ${failures} -gt 0 ]]; then
  echo "FAIL: release-scripts-smoke had ${failures} failure(s)" >&2
  exit 1
fi

echo "PASS: release-scripts-smoke"
