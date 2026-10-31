#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKAGE_SH="${ROOT_DIR}/scripts/release/package.sh"
PUBLISH_SH="${ROOT_DIR}/scripts/release/publish.sh"
TMP_DIR="$(mktemp -d -t homeboy-wp-provenance.XXXXXX)"
trap 'rm -rf "${TMP_DIR}"' EXIT

PROJECT="${TMP_DIR}/project"
RUNTIME="${TMP_DIR}/runtime"
BIN_DIR="${TMP_DIR}/bin"
mkdir -p "${PROJECT}/build" "${RUNTIME}/scripts/build" "${RUNTIME}/scripts/release" "${BIN_DIR}"
cp "${PACKAGE_SH}" "${RUNTIME}/scripts/release/package.sh"
cp "${PUBLISH_SH}" "${RUNTIME}/scripts/release/publish.sh"
cp "${ROOT_DIR}/scripts/release/verify-artifact-version.sh" "${RUNTIME}/scripts/release/verify-artifact-version.sh"
chmod +x "${RUNTIME}/scripts/release/package.sh" "${RUNTIME}/scripts/release/publish.sh"

cat > "${RUNTIME}/scripts/build/build.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
test -f build/demo-plugin.zip
SH
chmod +x "${RUNTIME}/scripts/build/build.sh"

python3 -c "
import zipfile
with zipfile.ZipFile('${PROJECT}/build/demo-plugin.zip', 'w') as z:
    z.writestr('demo-plugin/demo-plugin.php', '<?php\n/**\n * Plugin Name: Demo Plugin\n * Version: 1.2.3\n */')
"

cat > "${PROJECT}/homeboy.json" <<'JSON'
{
  "extensions": {
    "wordpress": {
      "settings": {
        "release_provenance_command": "jq -n --arg root \"$HOMEBOY_WORDPRESS_RELEASE_PACKAGE_ROOT\" --arg version \"$HOMEBOY_WORDPRESS_RELEASE_SOURCE_VERSION\" --arg tag \"$HOMEBOY_WORDPRESS_RELEASE_SOURCE_TAG\" --arg commit \"$HOMEBOY_WORDPRESS_RELEASE_SOURCE_COMMIT\" --arg zip \"$HOMEBOY_WORDPRESS_RELEASE_ZIP_PATH\" --arg output \"$HOMEBOY_WORDPRESS_RELEASE_OUTPUT_PATH\" --arg sidecar \"$HOMEBOY_WORDPRESS_RELEASE_SIDECAR_PATH\" '{root:$root,version:$version,tag:$tag,commit:$commit,zip:$zip,output:$output,sidecar:$sidecar}' > \"$HOMEBOY_WORDPRESS_RELEASE_SIDECAR_PATH\""
      }
    }
  }
}
JSON

PAYLOAD='{"release":{"version":"1.2.3","tag":"v1.2.3","commit":"abc123","component_id":"demo-plugin"}}'
PACKAGE_OUTPUT="$(cd "${PROJECT}" && HOMEBOY_COMPONENT_ID=demo-plugin HOMEBOY_SETTINGS_JSON="${PAYLOAD}" "${RUNTIME}/scripts/release/package.sh")"
SIDECAR="${PROJECT}/build/demo-plugin.provenance.json"
PROJECT_REALPATH="$(cd "${PROJECT}" && pwd -P)"

printf '%s' "${PACKAGE_OUTPUT}" | jq -e 'length == 2 and .[1].type == "wordpress-provenance"' >/dev/null
jq -e --arg root "${PROJECT_REALPATH}" '
  .root == $root and
  .version == "1.2.3" and
  .tag == "v1.2.3" and
  .commit == "abc123" and
  (.zip | startswith($root + "/")) and
  (.output | startswith($root + "/")) and
  (.sidecar | startswith($root + "/")) and
  .homeboy_wordpress_release_provenance.zip.sha256 != ""
' "${SIDECAR}" >/dev/null
printf 'OK: provenance hook receives bounded root, source coordinates, and output paths\n'

FAIL_PROJECT="${TMP_DIR}/failure"
cp -R "${PROJECT}" "${FAIL_PROJECT}"
jq '.extensions.wordpress.settings.release_provenance_command = "exit 17"' "${FAIL_PROJECT}/homeboy.json" > "${FAIL_PROJECT}/homeboy.json.tmp"
mv "${FAIL_PROJECT}/homeboy.json.tmp" "${FAIL_PROJECT}/homeboy.json"
if (cd "${FAIL_PROJECT}" && HOMEBOY_COMPONENT_ID=demo-plugin HOMEBOY_SETTINGS_JSON="${PAYLOAD}" "${RUNTIME}/scripts/release/package.sh" >/dev/null 2>&1); then
  echo 'FAIL: failing provenance command did not abort package' >&2
  exit 1
fi
printf 'OK: failing provenance hook aborts package\n'

# The hook cannot escape the fixed sidecar destination through a symlink.
EXTERNAL_DIR="${TMP_DIR}/external"
mkdir -p "${EXTERNAL_DIR}"
printf '{}' > "${EXTERNAL_DIR}/sidecar.json"
SIDECAR_ESCAPE_PROJECT="${TMP_DIR}/sidecar-escape"
cp -R "${PROJECT}" "${SIDECAR_ESCAPE_PROJECT}"
jq --arg external "${EXTERNAL_DIR}/sidecar.json" '.extensions.wordpress.settings.release_provenance_command = "ln -s \($external) \"$HOMEBOY_WORDPRESS_RELEASE_SIDECAR_PATH\""' "${SIDECAR_ESCAPE_PROJECT}/homeboy.json" > "${SIDECAR_ESCAPE_PROJECT}/homeboy.json.tmp"
mv "${SIDECAR_ESCAPE_PROJECT}/homeboy.json.tmp" "${SIDECAR_ESCAPE_PROJECT}/homeboy.json"
if (cd "${SIDECAR_ESCAPE_PROJECT}" && HOMEBOY_COMPONENT_ID=demo-plugin HOMEBOY_SETTINGS_JSON="${PAYLOAD}" "${RUNTIME}/scripts/release/package.sh" >/dev/null 2>&1); then
  echo 'FAIL: symlink-escaped sidecar did not abort package' >&2
  exit 1
fi

# The hook cannot replace the staged ZIP with a symlink to an external output.
ZIP_ESCAPE_PROJECT="${TMP_DIR}/zip-escape"
cp -R "${PROJECT}" "${ZIP_ESCAPE_PROJECT}"
mkdir -p "${EXTERNAL_DIR}/zip"
cp "${ZIP_ESCAPE_PROJECT}/build/demo-plugin.zip" "${EXTERNAL_DIR}/zip/demo-plugin.zip"
jq --arg external "${EXTERNAL_DIR}/zip/demo-plugin.zip" '.extensions.wordpress.settings.release_provenance_command = "rm \"$HOMEBOY_WORDPRESS_RELEASE_ZIP_PATH\"; ln -s \($external) \"$HOMEBOY_WORDPRESS_RELEASE_ZIP_PATH\"; printf \"{}\" > \"$HOMEBOY_WORDPRESS_RELEASE_SIDECAR_PATH\""' "${ZIP_ESCAPE_PROJECT}/homeboy.json" > "${ZIP_ESCAPE_PROJECT}/homeboy.json.tmp"
mv "${ZIP_ESCAPE_PROJECT}/homeboy.json.tmp" "${ZIP_ESCAPE_PROJECT}/homeboy.json"
if (cd "${ZIP_ESCAPE_PROJECT}" && HOMEBOY_COMPONENT_ID=demo-plugin HOMEBOY_SETTINGS_JSON="${PAYLOAD}" "${RUNTIME}/scripts/release/package.sh" >/dev/null 2>&1); then
  echo 'FAIL: symlink-escaped ZIP did not abort package' >&2
  exit 1
fi

# Replacing the post-hook output directory with an external symlink is also
# rejected after command execution, rather than trusting its initial location.
OUTPUT_ESCAPE_PROJECT="${TMP_DIR}/output-escape"
cp -R "${PROJECT}" "${OUTPUT_ESCAPE_PROJECT}"
mkdir -p "${EXTERNAL_DIR}/output"
jq --arg external "${EXTERNAL_DIR}/output" '.extensions.wordpress.settings.release_provenance_command = "mv \"$HOMEBOY_WORDPRESS_RELEASE_ZIP_PATH\" \($external)/demo-plugin.zip; rmdir \"$HOMEBOY_WORDPRESS_RELEASE_OUTPUT_PATH\"; ln -s \($external) \"$HOMEBOY_WORDPRESS_RELEASE_OUTPUT_PATH\"; printf \"{}\" > \"$HOMEBOY_WORDPRESS_RELEASE_SIDECAR_PATH\""' "${OUTPUT_ESCAPE_PROJECT}/homeboy.json" > "${OUTPUT_ESCAPE_PROJECT}/homeboy.json.tmp"
mv "${OUTPUT_ESCAPE_PROJECT}/homeboy.json.tmp" "${OUTPUT_ESCAPE_PROJECT}/homeboy.json"
if (cd "${OUTPUT_ESCAPE_PROJECT}" && HOMEBOY_COMPONENT_ID=demo-plugin HOMEBOY_SETTINGS_JSON="${PAYLOAD}" "${RUNTIME}/scripts/release/package.sh" >/dev/null 2>&1); then
  echo 'FAIL: post-hook output directory escape did not abort package' >&2
  exit 1
fi
printf 'OK: external, symlink, and post-hook output containment are enforced\n'

cat > "${BIN_DIR}/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${HOMEBOY_GH_LOG}"
if [[ "$1 $2" == "release upload" ]] && [[ -n "${HOMEBOY_GH_FAIL_ASSET:-}" ]] && [[ "$*" == *"${HOMEBOY_GH_FAIL_ASSET}"* ]]; then
  exit 1
fi
if [[ "$1 $2" == "release view" ]]; then
  if [[ -n "${HOMEBOY_GH_ASSETS_JSON:-}" ]]; then
    printf '%s\n' "${HOMEBOY_GH_ASSETS_JSON}"
  else
    printf '%s\n' '{"assets":[]}'
  fi
fi
SH
chmod +x "${BIN_DIR}/gh"

# Mutating the staged ZIP after the hook has sealed its digest must prevent
# both the ZIP and sidecar from reaching the release upload command.
printf 'mutation' >> "${PROJECT}/build/demo-plugin.zip"
if (cd "${PROJECT}" && PATH="${BIN_DIR}:${PATH}" HOMEBOY_GH_LOG="${TMP_DIR}/mutation-gh.log" GITHUB_REPOSITORY=example/demo-plugin HOMEBOY_SETTINGS_JSON="${PAYLOAD}" "${RUNTIME}/scripts/release/publish.sh" >/dev/null 2>&1); then
  echo 'FAIL: post-hook ZIP mutation did not abort publish' >&2
  exit 1
fi
test ! -e "${TMP_DIR}/mutation-gh.log"
printf 'OK: post-hook ZIP mutation invalidates provenance before upload\n'

# The sidecar schema and immutable source coordinates must match the exact
# release payload even when the ZIP digest remains valid.
PACKAGE_OUTPUT="$(cd "${PROJECT}" && HOMEBOY_COMPONENT_ID=demo-plugin HOMEBOY_SETTINGS_JSON="${PAYLOAD}" "${RUNTIME}/scripts/release/package.sh")"
jq '.homeboy_wordpress_release_provenance.source.commit = "wrong"' "${SIDECAR}" > "${SIDECAR}.tmp"
mv "${SIDECAR}.tmp" "${SIDECAR}"
if (cd "${PROJECT}" && PATH="${BIN_DIR}:${PATH}" HOMEBOY_GH_LOG="${TMP_DIR}/source-mismatch-gh.log" GITHUB_REPOSITORY=example/demo-plugin HOMEBOY_SETTINGS_JSON="${PAYLOAD}" "${RUNTIME}/scripts/release/publish.sh" >/dev/null 2>&1); then
  echo 'FAIL: source-mismatched sidecar did not abort publish' >&2
  exit 1
fi
test ! -e "${TMP_DIR}/source-mismatch-gh.log"
PACKAGE_OUTPUT="$(cd "${PROJECT}" && HOMEBOY_COMPONENT_ID=demo-plugin HOMEBOY_SETTINGS_JSON="${PAYLOAD}" "${RUNTIME}/scripts/release/package.sh")"
jq '.homeboy_wordpress_release_provenance.version = 2' "${SIDECAR}" > "${SIDECAR}.tmp"
mv "${SIDECAR}.tmp" "${SIDECAR}"
if (cd "${PROJECT}" && PATH="${BIN_DIR}:${PATH}" HOMEBOY_GH_LOG="${TMP_DIR}/schema-mismatch-gh.log" GITHUB_REPOSITORY=example/demo-plugin HOMEBOY_SETTINGS_JSON="${PAYLOAD}" "${RUNTIME}/scripts/release/publish.sh" >/dev/null 2>&1); then
  echo 'FAIL: unsupported sidecar schema version did not abort publish' >&2
  exit 1
fi
test ! -e "${TMP_DIR}/schema-mismatch-gh.log"
printf 'OK: sidecar schema and source coordinates are verified\n'

# Rebuild and reseal, then assert the publisher uploads both assets and its
# receipt binds the independently computed ZIP and sidecar digests.
python3 -c "
import zipfile
with zipfile.ZipFile('${PROJECT}/build/demo-plugin.zip', 'w') as z:
    z.writestr('demo-plugin/demo-plugin.php', '<?php\n/**\n * Plugin Name: Demo Plugin\n * Version: 1.2.3\n */')
"
PACKAGE_OUTPUT="$(cd "${PROJECT}" && HOMEBOY_COMPONENT_ID=demo-plugin HOMEBOY_SETTINGS_JSON="${PAYLOAD}" "${RUNTIME}/scripts/release/package.sh")"
PUBLISH_PAYLOAD="$(printf '%s' "${PAYLOAD}" | jq --arg zip "${PROJECT}/build/demo-plugin.zip" --arg sidecar "${SIDECAR}" '.release.artifacts = [{path:$zip,type:"wordpress-zip"},{path:$sidecar,type:"wordpress-provenance"}]')"
PUBLISH_OUTPUT="$(cd "${PROJECT}" && PATH="${BIN_DIR}:${PATH}" HOMEBOY_GH_LOG="${TMP_DIR}/upload-gh.log" HOMEBOY_GH_ASSETS_JSON='{"assets":[{"name":"demo-plugin.zip"},{"name":"demo-plugin.provenance.json"}]}' GITHUB_REPOSITORY=example/demo-plugin HOMEBOY_SETTINGS_JSON="${PUBLISH_PAYLOAD}" "${RUNTIME}/scripts/release/publish.sh")"
grep -F "release upload v1.2.3 ${SIDECAR} --clobber --repo example/demo-plugin" "${TMP_DIR}/upload-gh.log" >/dev/null
grep -F "release upload v1.2.3 ${PROJECT}/build/demo-plugin.zip --clobber --repo example/demo-plugin" "${TMP_DIR}/upload-gh.log" >/dev/null
ZIP_SHA256="$(shasum -a 256 "${PROJECT}/build/demo-plugin.zip")"
ZIP_SHA256="${ZIP_SHA256%% *}"
SIDECAR_SHA256="$(shasum -a 256 "${SIDECAR}")"
SIDECAR_SHA256="${SIDECAR_SHA256%% *}"
printf '%s' "${PUBLISH_OUTPUT}" | tail -1 | jq -e --arg zip "${ZIP_SHA256}" --arg sidecar "${SIDECAR_SHA256}" '
  .success == true and .provenance.zip_sha256 == $zip and .provenance.sidecar_sha256 == $sidecar
' >/dev/null

# A sidecar upload failure never uploads the ZIP and rolls back both names so
# a stale sidecar or unprovenanced ZIP cannot remain from this attempt.
if (cd "${PROJECT}" && PATH="${BIN_DIR}:${PATH}" HOMEBOY_GH_LOG="${TMP_DIR}/sidecar-failure-gh.log" HOMEBOY_GH_FAIL_ASSET="demo-plugin.provenance.json" GITHUB_REPOSITORY=example/demo-plugin HOMEBOY_SETTINGS_JSON="${PUBLISH_PAYLOAD}" "${RUNTIME}/scripts/release/publish.sh" >/dev/null 2>&1); then
  echo 'FAIL: sidecar upload failure did not abort publish' >&2
  exit 1
fi
grep -F "release upload v1.2.3 ${SIDECAR}" "${TMP_DIR}/sidecar-failure-gh.log" >/dev/null
if grep -F "release upload v1.2.3 ${PROJECT}/build/demo-plugin.zip" "${TMP_DIR}/sidecar-failure-gh.log" >/dev/null; then
  echo 'FAIL: ZIP uploaded after sidecar failure' >&2
  exit 1
fi
grep -F 'release delete-asset v1.2.3 demo-plugin.zip --yes --repo example/demo-plugin' "${TMP_DIR}/sidecar-failure-gh.log" >/dev/null
grep -F 'release delete-asset v1.2.3 demo-plugin.provenance.json --yes --repo example/demo-plugin' "${TMP_DIR}/sidecar-failure-gh.log" >/dev/null

# If the second-leg ZIP upload fails after sidecar success, both names are
# removed, preventing a stale sidecar from representing a failed release.
if (cd "${PROJECT}" && PATH="${BIN_DIR}:${PATH}" HOMEBOY_GH_LOG="${TMP_DIR}/zip-failure-gh.log" HOMEBOY_GH_FAIL_ASSET="demo-plugin.zip" GITHUB_REPOSITORY=example/demo-plugin HOMEBOY_SETTINGS_JSON="${PUBLISH_PAYLOAD}" "${RUNTIME}/scripts/release/publish.sh" >/dev/null 2>&1); then
  echo 'FAIL: ZIP upload failure did not abort publish' >&2
  exit 1
fi
grep -F "release upload v1.2.3 ${SIDECAR}" "${TMP_DIR}/zip-failure-gh.log" >/dev/null
grep -F "release upload v1.2.3 ${PROJECT}/build/demo-plugin.zip" "${TMP_DIR}/zip-failure-gh.log" >/dev/null
grep -F 'release delete-asset v1.2.3 demo-plugin.zip --yes --repo example/demo-plugin' "${TMP_DIR}/zip-failure-gh.log" >/dev/null
grep -F 'release delete-asset v1.2.3 demo-plugin.provenance.json --yes --repo example/demo-plugin' "${TMP_DIR}/zip-failure-gh.log" >/dev/null
printf 'OK: failed provenance publication is rolled back\n'
printf 'PASS: release provenance contract smoke\n'
