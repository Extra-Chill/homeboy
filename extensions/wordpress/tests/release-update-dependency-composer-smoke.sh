#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UPDATE_DEP_SH="${SCRIPT_DIR}/../scripts/release/update-dependency.sh"
WORK_DIR="$(mktemp -d -t homeboy-composer-update.XXXXXX)"
trap 'rm -rf "${WORK_DIR}"' EXIT

make_package() {
  local package_dir="$1" version="$2" archive="$3"
  mkdir -p "${package_dir}"
  cat > "${package_dir}/composer.json" <<JSON
{"name":"acme/library","version":"${version}","type":"library"}
JSON
  printf '%s\n' "<?php" > "${package_dir}/Library.php"
  (cd "${package_dir}" && zip -q -r "${archive}" composer.json Library.php)
}

ARTIFACTS="${WORK_DIR}/artifacts"
mkdir -p "${ARTIFACTS}"
make_package "${WORK_DIR}/pkg-1.0" "1.0.0" "${ARTIFACTS}/acme-library-1.0.0.zip"
make_package "${WORK_DIR}/pkg-1.1" "1.1.0" "${ARTIFACTS}/acme-library-1.1.0.zip"

BETA_ARTIFACTS="${WORK_DIR}/beta-artifacts"
mkdir -p "${BETA_ARTIFACTS}"
make_package "${WORK_DIR}/pkg-beta-1.0" "1.0.0" "${BETA_ARTIFACTS}/acme-library-1.0.0.zip"
make_package "${WORK_DIR}/pkg-beta-1.2" "1.2.0-beta" "${BETA_ARTIFACTS}/acme-library-1.2.0-beta.zip"

NORMAL="${WORK_DIR}/normal"
mkdir -p "${NORMAL}"
cat > "${NORMAL}/composer.json" <<JSON
{
  "name": "acme/app",
  "require": { "acme/library": "1.0.0" },
  "repositories": [{ "type": "artifact", "url": "${ARTIFACTS}" }]
}
JSON
(cd "${NORMAL}" && composer install --no-interaction --no-scripts --no-progress >/dev/null)

NORMAL_PAYLOAD='{"dependency":{"package":"acme/library","version":"1.1.0"}}'
normal_out="$(cd "${NORMAL}" && HOMEBOY_SETTINGS_JSON="${NORMAL_PAYLOAD}" "${UPDATE_DEP_SH}")"
echo "${normal_out}" | jq -e '.success == true and .version == "1.1.0" and .changed == true' >/dev/null
jq -e '.packages[] | select(.name == "acme/library") | .version == "1.1.0"' "${NORMAL}/composer.lock" >/dev/null

DISCOVERY="${WORK_DIR}/discovery"
mkdir -p "${DISCOVERY}"
cat > "${DISCOVERY}/composer.json" <<JSON
{
  "name": "acme/discovery-app",
  "require": { "acme/library": "1.0.0", "php": ">=8.1" },
  "repositories": [{ "type": "artifact", "url": "${ARTIFACTS}" }]
}
JSON
(cd "${DISCOVERY}" && composer install --no-interaction --no-scripts --no-progress >/dev/null)
discovery_out="$(cd "${DISCOVERY}" && HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"acme/library","version":"latest","discovery_constraint":"^1.0","allow_constraint_replacement":true}}' "${UPDATE_DEP_SH}")"
echo "${discovery_out}" | jq -e '.success == true and .version == "1.1.0" and .changed == true' >/dev/null
jq -e '.require["acme/library"] == "1.1.0" and .require.php == ">=8.1"' "${DISCOVERY}/composer.json" >/dev/null
COMPOSER_BIN="$(command -v composer)"
FAKE_BIN="${WORK_DIR}/fake-bin"
mkdir -p "${FAKE_BIN}"
cat > "${FAKE_BIN}/composer" <<SH
#!/usr/bin/env bash
printf '%s\n' 'benign composer stdout'
exec "${COMPOSER_BIN}" "\$@"
SH
chmod +x "${FAKE_BIN}/composer"

discovery_json_hash="$(shasum "${DISCOVERY}/composer.json" | cut -d' ' -f1)"
discovery_lock_hash="$(shasum "${DISCOVERY}/composer.lock" | cut -d' ' -f1)"
verify_out="$(cd "${DISCOVERY}" && PATH="${FAKE_BIN}:${PATH}" HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"acme/library","version":"1.1.0","mode":"verify"}}' "${UPDATE_DEP_SH}")"
echo "${verify_out}" | jq -e '.success == true and .verification_only == true and .changed == false' >/dev/null
[[ "${discovery_json_hash}" == "$(shasum "${DISCOVERY}/composer.json" | cut -d' ' -f1)" ]]
[[ "${discovery_lock_hash}" == "$(shasum "${DISCOVERY}/composer.lock" | cut -d' ' -f1)" ]]
(cd "${DISCOVERY}" && composer validate --no-check-publish --check-lock --no-interaction >/dev/null 2>&1)

PRERELEASE="${WORK_DIR}/prerelease"
mkdir -p "${PRERELEASE}"
cat > "${PRERELEASE}/composer.json" <<JSON
{
  "name": "acme/prerelease-app",
  "minimum-stability": "dev",
  "require": { "acme/library": "1.0.0" },
  "repositories": [{ "type": "artifact", "url": "${BETA_ARTIFACTS}" }]
}
JSON
(cd "${PRERELEASE}" && composer install --no-interaction --no-scripts --no-progress >/dev/null)
BETA_FAKE_BIN="${WORK_DIR}/beta-fake-bin"
mkdir -p "${BETA_FAKE_BIN}"
cat > "${BETA_FAKE_BIN}/composer" <<SH
#!/usr/bin/env bash
"${COMPOSER_BIN}" "\$@"
if [[ "\${1:-}" == "update" ]]; then
  jq '(.packages // []) |= map(if .name == "acme/library" then .version = "1.2.0-beta" else . end)' composer.lock > composer.lock.beta
  mv composer.lock.beta composer.lock
fi
SH
chmod +x "${BETA_FAKE_BIN}/composer"
set +e
prerelease_err="$(cd "${PRERELEASE}" && PATH="${BETA_FAKE_BIN}:${PATH}" HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"acme/library","version":"latest","discovery_constraint":"^1.0","allow_constraint_replacement":true}}' "${UPDATE_DEP_SH}" 2>&1 >/dev/null)"
prerelease_status=$?
set -e
[[ ${prerelease_status} -ne 0 ]]
printf '%s\n' "${prerelease_err}" | grep -q 'resolved prerelease'

normal_json_hash="$(shasum "${NORMAL}/composer.json" | cut -d' ' -f1)"
normal_lock_hash="$(shasum "${NORMAL}/composer.lock" | cut -d' ' -f1)"
noop_out="$(cd "${NORMAL}" && HOMEBOY_SETTINGS_JSON="${NORMAL_PAYLOAD}" "${UPDATE_DEP_SH}")"
echo "${noop_out}" | jq -e '.success == true and .changed == false and .composer_refreshed == false' >/dev/null
[[ "${normal_json_hash}" == "$(shasum "${NORMAL}/composer.json" | cut -d' ' -f1)" ]]
[[ "${normal_lock_hash}" == "$(shasum "${NORMAL}/composer.lock" | cut -d' ' -f1)" ]]

set +e
downgrade_err="$(cd "${NORMAL}" && HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"acme/library","version":"1.0.0"}}' "${UPDATE_DEP_SH}" 2>&1 >/dev/null)"
downgrade_status=$?
set -e
[[ ${downgrade_status} -ne 0 ]]
printf '%s\n' "${downgrade_err}" | grep -q 'refusing to downgrade'
[[ "${normal_json_hash}" == "$(shasum "${NORMAL}/composer.json" | cut -d' ' -f1)" ]]
[[ "${normal_lock_hash}" == "$(shasum "${NORMAL}/composer.lock" | cut -d' ' -f1)" ]]

set +e
failure_err="$(cd "${NORMAL}" && HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"acme/library","version":"9.0.0"}}' "${UPDATE_DEP_SH}" 2>&1 >/dev/null)"
failure_status=$?
set -e
[[ ${failure_status} -ne 0 ]]
[[ "${normal_json_hash}" == "$(shasum "${NORMAL}/composer.json" | cut -d' ' -f1)" ]]
[[ "${normal_lock_hash}" == "$(shasum "${NORMAL}/composer.lock" | cut -d' ' -f1)" ]]
printf '%s\n' "${failure_err}" | grep -q 'left unchanged'

INLINE="${WORK_DIR}/inline"
mkdir -p "${INLINE}"
cat > "${INLINE}/composer.json" <<JSON
{
  "name": "acme/inline-app",
  "require": { "acme/library": "1.0.0" },
  "repositories": [{ "type": "package", "package": {
    "name": "acme/library", "version": "1.0.0",
    "dist": { "type": "zip", "url": "${ARTIFACTS}/acme-library-1.0.0.zip", "reference": "old" },
    "source": { "type": "git", "url": "https://example.test/acme/library.git", "reference": "old" }
  }}]
}
JSON
(cd "${INLINE}" && composer install --no-interaction --no-scripts --no-progress >/dev/null)
set +e
inline_discovery_err="$(cd "${INLINE}" && HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"acme/library","version":"latest"}}' "${UPDATE_DEP_SH}" 2>&1 >/dev/null)"
inline_discovery_status=$?
set -e
[[ ${inline_discovery_status} -ne 0 ]]
printf '%s\n' "${inline_discovery_err}" | grep -q 'unavailable for one-item inline package metadata'
inline_out="$(cd "${INLINE}" && HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"acme/library","version":"1.1.0","sha":"dispatch","expected_source":"https://example.test/acme/library.git","expected_source_sha":"dispatch"}}' "${UPDATE_DEP_SH}")"
echo "${inline_out}" | jq -e '.success == true and .version == "1.1.0"' >/dev/null
jq -e '.repositories[0].package.source.url == "https://example.test/acme/library.git" and .repositories[0].package.source.reference == "dispatch" and .repositories[0].package.dist.reference == "dispatch"' "${INLINE}/composer.json" >/dev/null
jq -e '.packages[] | select(.name == "acme/library") | .version == "1.1.0"' "${INLINE}/composer.lock" >/dev/null
inline_changed_out="$(cd "${INLINE}" && HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"acme/library","version":"1.1.0","sha":"dispatch-2","expected_source":"https://example.test/acme/library.git","expected_source_sha":"dispatch-2"}}' "${UPDATE_DEP_SH}")"
echo "${inline_changed_out}" | jq -e '.success == true and .changed == true' >/dev/null
jq -e '.repositories[0].package.source.reference == "dispatch-2" and .repositories[0].package.dist.reference == "dispatch-2"' "${INLINE}/composer.json" >/dev/null

# Packages published from "v"-prefixed tags report "v1.2.3" in composer.lock,
# while callers request the bare "1.2.3". Composer treats these as the same
# release, so the action must too: an exact bare request resolves, a repeated
# request is a no-op, and a lock ahead of the request is still a downgrade.
V_ARTIFACTS="${WORK_DIR}/v-artifacts"
mkdir -p "${V_ARTIFACTS}"
make_package "${WORK_DIR}/pkg-v-1.0" "v1.0.0" "${V_ARTIFACTS}/acme-library-v1.0.0.zip"
make_package "${WORK_DIR}/pkg-v-1.1" "v1.1.0" "${V_ARTIFACTS}/acme-library-v1.1.0.zip"

VPREFIX="${WORK_DIR}/v-prefix"
mkdir -p "${VPREFIX}"
cat > "${VPREFIX}/composer.json" <<JSON
{
  "name": "acme/v-prefix-app",
  "require": { "acme/library": "v1.0.0" },
  "repositories": [{ "type": "artifact", "url": "${V_ARTIFACTS}" }]
}
JSON
(cd "${VPREFIX}" && composer install --no-interaction --no-scripts --no-progress >/dev/null)
jq -e '.packages[] | select(.name == "acme/library") | .version == "v1.0.0"' "${VPREFIX}/composer.lock" >/dev/null

vprefix_out="$(cd "${VPREFIX}" && HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"acme/library","version":"1.1.0"}}' "${UPDATE_DEP_SH}")"
echo "${vprefix_out}" | jq -e '.success == true and .changed == true' >/dev/null
jq -e '.packages[] | select(.name == "acme/library") | .version == "v1.1.0"' "${VPREFIX}/composer.lock" >/dev/null

vprefix_json_hash="$(shasum "${VPREFIX}/composer.json" | cut -d' ' -f1)"
vprefix_lock_hash="$(shasum "${VPREFIX}/composer.lock" | cut -d' ' -f1)"
vprefix_repeat_out="$(cd "${VPREFIX}" && HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"acme/library","version":"1.1.0"}}' "${UPDATE_DEP_SH}")"
echo "${vprefix_repeat_out}" | jq -e '.success == true and .changed == false' >/dev/null
[[ "${vprefix_json_hash}" == "$(shasum "${VPREFIX}/composer.json" | cut -d' ' -f1)" ]]
[[ "${vprefix_lock_hash}" == "$(shasum "${VPREFIX}/composer.lock" | cut -d' ' -f1)" ]]

set +e
vprefix_downgrade_err="$(cd "${VPREFIX}" && HOMEBOY_SETTINGS_JSON='{"dependency":{"package":"acme/library","version":"1.0.0"}}' "${UPDATE_DEP_SH}" 2>&1 >/dev/null)"
vprefix_downgrade_status=$?
set -e
[[ ${vprefix_downgrade_status} -ne 0 ]]
printf '%s\n' "${vprefix_downgrade_err}" | grep -q 'refusing to downgrade'
[[ "${vprefix_lock_hash}" == "$(shasum "${VPREFIX}/composer.lock" | cut -d' ' -f1)" ]]

echo "PASS: release-update-dependency-composer-smoke"
