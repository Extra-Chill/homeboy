#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXTENSION_DIR="${ROOT_DIR}/wordpress"
CORE_RUNTIME_DIR="${ROOT_DIR}/../crates/homeboy-core/src/extension/runtime"
export HOMEBOY_RUNTIME_RUNNER_PRELUDE="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-${CORE_RUNTIME_DIR}/runner-prelude.sh}"
export HOMEBOY_RUNTIME_RESOLVE_CONTEXT="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${CORE_RUNTIME_DIR}/resolve-context.sh}"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-release-package-cwd.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

component_dir="${TMP_DIR}/component"
mkdir -p "${component_dir}/includes"
component_dir="$(cd "${component_dir}" && pwd)"

# Write an unversioned companion before the canonical root entrypoint.
printf '%s\n' '<?php' '/* Plugin Name: Companion */' > "${component_dir}/a-companion.php"
cat > "${component_dir}/cwd-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: CWD Plugin
 * Version: 1.2.3
 */
PHP

printf '%s\n' '<?php' > "${component_dir}/includes/bootstrap.php"

cat > "${component_dir}/package.json" <<'JSON'
{
  "scripts": {
    "build": "pwd > npm-build-cwd.txt"
  }
}
JSON

payload="$(jq -cn \
  --arg path "${component_dir}" \
  '{release:{version:"1.2.3",tag:"cwd-plugin-v1.2.3",component_id:"cwd-plugin",local_path:$path}}')"

(
  cd "${TMP_DIR}"
  HOMEBOY_COMPONENT_ID="cwd-plugin" \
  HOMEBOY_SKIP_TESTS=1 \
  HOMEBOY_SETTINGS_JSON="${payload}" \
    bash "${EXTENSION_DIR}/scripts/release/package.sh" > "${TMP_DIR}/package.json.out" 2> "${TMP_DIR}/package.stderr"
) || { cat "${TMP_DIR}/package.stderr" >&2; exit 1; }
grep -q 'Detected WordPress plugin: cwd-plugin.php' "${TMP_DIR}/package.stderr"
if grep -Eq 'signal 13|Broken pipe' "${TMP_DIR}/package.stderr"; then
  cat "${TMP_DIR}/package.stderr" >&2
  exit 1
fi
unzip -Z1 "${component_dir}/build/cwd-plugin.zip" | grep -x 'cwd-plugin/a-companion.php' >/dev/null

# A standalone run uses version_targets even when another versioned header
# matches the checkout basename. Both build and verification must use it.
standalone_dir="${TMP_DIR}/standalone"
mkdir "${standalone_dir}"
printf '%s\n' '<?php' '/* Plugin Name: Versioned Companion' ' * Version: 9.0.0' ' */' > "${standalone_dir}/standalone.php"
cp "${component_dir}/cwd-plugin.php" "${standalone_dir}/z-main.php"
printf '%s\n' '{"version_targets":[{"file":"z-main.php"},{"file":"z-main.php"},{"file":"composer.json"}]}' > "${standalone_dir}/homeboy.json"
(
  cd "${standalone_dir}"
  unset HOMEBOY_COMPONENT_ID HOMEBOY_SETTINGS_JSON
  HOMEBOY_SKIP_TESTS=1 bash "${EXTENSION_DIR}/scripts/release/package.sh" > "${TMP_DIR}/standalone.json.out" 2> "${TMP_DIR}/standalone.stderr"
) || { cat "${TMP_DIR}/standalone.stderr" >&2; exit 1; }

jq -e '.[0].path == "build/z-main.zip"' "${TMP_DIR}/standalone.json.out" >/dev/null
grep -q 'Verified build/z-main.zip contains version 1.2.3' "${TMP_DIR}/standalone.stderr"

# Without a matching component ID or version targets, two versioned entries
# are ambiguous rather than silently selecting the first filename.
rm "${standalone_dir}/homeboy.json"
if (
  cd "${standalone_dir}"
  HOMEBOY_COMPONENT_ID=unknown HOMEBOY_SETTINGS_JSON='{}' \
    bash "${EXTENSION_DIR}/scripts/release/package.sh" > /dev/null 2> "${TMP_DIR}/ambiguous.stderr"
); then
  echo 'Expected packaging to reject ambiguous plugin entrypoints' >&2
  exit 1
fi
grep -q 'ambiguous WordPress plugin entrypoint' "${TMP_DIR}/ambiguous.stderr"

if [[ ! -f "${component_dir}/build/cwd-plugin.zip" ]]; then
  echo "Expected package script to build artifact in release.local_path" >&2
  sed 's/^/  /' "${TMP_DIR}/package.stderr" >&2
  exit 1
fi

if [[ "$(cat "${component_dir}/npm-build-cwd.txt")" != "${component_dir}" ]]; then
  echo "Expected npm build to run from release.local_path" >&2
  echo "  got: $(cat "${component_dir}/npm-build-cwd.txt")" >&2
  echo "  want: ${component_dir}" >&2
  exit 1
fi

if ! jq -e '.[0].path == "build/cwd-plugin.zip" and .[0].type == "wordpress-zip"' "${TMP_DIR}/package.json.out" >/dev/null; then
  echo "Expected release artifact JSON on stdout" >&2
  sed 's/^/  /' "${TMP_DIR}/package.json.out" >&2
  exit 1
fi

echo "WordPress release package cwd smoke passed."

nested_repo="${TMP_DIR}/nested-repo"
nested_component_dir="${nested_repo}/plugins/host/nested-plugin"
temp_package_dir="${TMP_DIR}/temp-package/nested-plugin"
mkdir -p "${nested_component_dir}/includes" "${temp_package_dir}"
nested_component_dir="$(cd "${nested_component_dir}" && pwd)"
temp_package_dir="$(cd "${temp_package_dir}" && pwd)"

printf '%s\n' 'repo-root-marker' > "${nested_repo}/repo-root.txt"
cat > "${nested_component_dir}/nested-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Nested Plugin
 * Version: 2.0.0
 */
PHP

printf '%s\n' '<?php' > "${nested_component_dir}/includes/bootstrap.php"
cat > "${nested_component_dir}/package.json" <<'JSON'
{
  "scripts": {
    "build": "cd ../../.. && test -f repo-root.txt && pwd > plugins/host/nested-plugin/repo-root-cwd.txt"
  }
}
JSON

cp "${nested_component_dir}/nested-plugin.php" "${temp_package_dir}/nested-plugin.php"

nested_payload="$(jq -cn \
  --arg tempPath "${temp_package_dir}" \
  --arg sourcePath "${nested_component_dir}" \
  '{release:{version:"2.0.0",tag:"nested-plugin-v2.0.0",component_id:"nested-plugin",local_path:$tempPath,source_path:$sourcePath}}')"

(
  cd "${TMP_DIR}"
  HOMEBOY_COMPONENT_ID="nested-plugin" \
  HOMEBOY_COMPONENT_PATH="${temp_package_dir}" \
  HOMEBOY_SKIP_TESTS=1 \
  HOMEBOY_SETTINGS_JSON="${nested_payload}" \
    bash "${EXTENSION_DIR}/scripts/release/package.sh" > "${TMP_DIR}/nested-package.json.out" 2> "${TMP_DIR}/nested-package.stderr"
)

if [[ ! -f "${nested_component_dir}/build/nested-plugin.zip" ]]; then
  echo "Expected nested package script to build artifact in HOMEBOY_COMPONENT_PATH" >&2
  sed 's/^/  /' "${TMP_DIR}/nested-package.stderr" >&2
  exit 1
fi

if [[ -f "${temp_package_dir}/build/nested-plugin.zip" ]]; then
  echo "Expected temp package directory not to receive the release artifact" >&2
  exit 1
fi

expected_repo_root="$(cd "${nested_repo}" && pwd)"
if [[ "$(cat "${nested_component_dir}/repo-root-cwd.txt")" != "${expected_repo_root}" ]]; then
  echo "Expected nested npm build to climb from real component path to repo root" >&2
  echo "  got: $(cat "${nested_component_dir}/repo-root-cwd.txt")" >&2
  echo "  want: ${expected_repo_root}" >&2
  exit 1
fi

if ! jq -e '.[0].path == "build/nested-plugin.zip" and .[0].type == "wordpress-zip"' "${TMP_DIR}/nested-package.json.out" >/dev/null; then
  echo "Expected nested release artifact JSON on stdout" >&2
  sed 's/^/  /' "${TMP_DIR}/nested-package.json.out" >&2
  exit 1
fi

echo "WordPress release package nested component cwd smoke passed."
