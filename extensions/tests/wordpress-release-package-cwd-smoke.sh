#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXTENSION_DIR="${ROOT_DIR}/wordpress"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
RESOLVE_CONTEXT_CORE_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/resolve-context.sh}"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-release-package-cwd.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

component_dir="${TMP_DIR}/component"
mkdir -p "${component_dir}/includes"
component_dir="$(cd "${component_dir}" && pwd)"

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
  HOMEBOY_RUNTIME_RESOLVE_CONTEXT="${RESOLVE_CONTEXT_CORE_HELPER}" \
  HOMEBOY_SKIP_TESTS=1 \
  HOMEBOY_SETTINGS_JSON="${payload}" \
    bash "${EXTENSION_DIR}/scripts/release/package.sh" > "${TMP_DIR}/package.json.out" 2> "${TMP_DIR}/package.stderr"
)

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
