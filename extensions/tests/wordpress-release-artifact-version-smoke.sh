#!/usr/bin/env bash
set -euo pipefail

# Smoke test for wordpress/scripts/release/verify-artifact-version.sh:
# the guard that refuses to publish a release ZIP whose internal plugin/theme
# version does not match the version the release is shipping (the
# data-machine-socials v0.14.0 stale-asset incident).

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERIFY="${ROOT_DIR}/wordpress/scripts/release/verify-artifact-version.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

make_plugin_zip() {
  local zip_path="$1"
  local version="$2"
  local stage="${TMP_DIR}/stage-$RANDOM"
  mkdir -p "${stage}/test-plugin"
  cat > "${stage}/test-plugin/test-plugin.php" <<EOF
<?php
/**
 * Plugin Name: Test Plugin
 * Version: ${version}
 */
EOF
  (cd "${stage}" && zip -q -r "${zip_path}" "test-plugin/")
  rm -rf "${stage}"
}

make_theme_zip() {
  local zip_path="$1"
  local version="$2"
  local stage="${TMP_DIR}/stage-$RANDOM"
  mkdir -p "${stage}/test-theme"
  cat > "${stage}/test-theme/style.css" <<EOF
/*
Theme Name: Test Theme
Version: ${version}
*/
EOF
  (cd "${stage}" && zip -q -r "${zip_path}" "test-theme/")
  rm -rf "${stage}"
}

# 1. Matching plugin version passes and prints the version.
make_plugin_zip "${TMP_DIR}/match.zip" "1.2.3"
got="$(bash "${VERIFY}" "${TMP_DIR}/match.zip" "1.2.3")"
if [[ "${got}" != "1.2.3" ]]; then
  echo "expected matched version output '1.2.3', got '${got}'" >&2
  exit 1
fi

# 2. Mismatched plugin version fails (the stale-artifact case).
make_plugin_zip "${TMP_DIR}/stale.zip" "0.8.1"
if bash "${VERIFY}" "${TMP_DIR}/stale.zip" "0.14.0" 2>/dev/null; then
  echo "stale artifact (0.8.1 vs expected 0.14.0) was not rejected" >&2
  exit 1
fi

# 3. Theme style.css fallback passes on match.
make_theme_zip "${TMP_DIR}/theme.zip" "2.0.0"
got="$(bash "${VERIFY}" "${TMP_DIR}/theme.zip" "2.0.0")"
if [[ "${got}" != "2.0.0" ]]; then
  echo "expected theme version output '2.0.0', got '${got}'" >&2
  exit 1
fi

# 4. Theme mismatch fails.
if bash "${VERIFY}" "${TMP_DIR}/theme.zip" "2.1.0" 2>/dev/null; then
  echo "stale theme artifact (2.0.0 vs expected 2.1.0) was not rejected" >&2
  exit 1
fi

# 5. ZIP without any version header fails rather than passing silently.
stage="${TMP_DIR}/stage-noheader"
mkdir -p "${stage}/mystery"
echo "<?php // no headers" > "${stage}/mystery/file.php"
(cd "${stage}" && zip -q -r "${TMP_DIR}/noheader.zip" "mystery/")
if bash "${VERIFY}" "${TMP_DIR}/noheader.zip" "1.0.0" 2>/dev/null; then
  echo "artifact without a version header was not rejected" >&2
  exit 1
fi

# 6. Missing artifact path fails.
if bash "${VERIFY}" "${TMP_DIR}/does-not-exist.zip" "1.0.0" 2>/dev/null; then
  echo "missing artifact was not rejected" >&2
  exit 1
fi

echo "wordpress release artifact version smoke passed"
