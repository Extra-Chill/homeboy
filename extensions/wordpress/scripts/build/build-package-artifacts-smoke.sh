#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RESOLVE_CONTEXT_CORE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-package-artifacts.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

assert_zip_contains() {
    local zip_file="$1"
    local expected="$2"
    if ! unzip -l "$zip_file" | grep -F -- "$expected" >/dev/null; then
        echo "Expected $zip_file to contain $expected" >&2
        unzip -l "$zip_file" >&2
        exit 1
    fi
}

assert_zip_not_contains() {
    local zip_file="$1"
    local unexpected="$2"
    if unzip -l "$zip_file" | grep -F -- "$unexpected" >/dev/null; then
        echo "Expected $zip_file not to contain $unexpected" >&2
        unzip -l "$zip_file" >&2
        exit 1
    fi
}

component_dir="${TMP_DIR}/package-artifact-plugin"
mkdir -p "${component_dir}/runtime/packages" "${component_dir}/runtime/nested/packages" "${component_dir}/runtime/omit" "${component_dir}/includes"
mkdir -p "${component_dir}/.git"

cat > "${component_dir}/package-artifact-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Package Artifact Plugin
 * Version: 1.0.0
 */
PHP

printf '%s\n' '<?php' > "${component_dir}/includes/bootstrap.php"
printf '%s\n' 'accidental release zip' > "${component_dir}/accidental.zip"
printf '%s\n' 'intentional package zip' > "${component_dir}/runtime/packages/static-site-importer.zip"
printf '%s\n' 'nested package zip' > "${component_dir}/runtime/nested/packages/nested.zip"
printf '%s\n' 'excluded runtime source' > "${component_dir}/runtime/omit/source.txt"
printf '%s\n' 'ref: refs/heads/main' > "${component_dir}/.git/HEAD"
ln -s "${component_dir}/runtime/packages/static-site-importer.zip" "${component_dir}/runtime/packages/linked.zip"

(
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/default-build.out"
)

default_zip="${component_dir}/build/package-artifact-plugin.zip"
assert_zip_contains "$default_zip" "package-artifact-plugin/package-artifact-plugin.php"
assert_zip_not_contains "$default_zip" "package-artifact-plugin/accidental.zip"
assert_zip_not_contains "$default_zip" "package-artifact-plugin/runtime/packages/static-site-importer.zip"
assert_zip_not_contains "$default_zip" "package-artifact-plugin/.git/HEAD"

(
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_artifacts":["runtime/**/packages/*.zip","runtime/packages/*.zip"],"package_excludes":["/runtime/omit/"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/included-build.out"
)

included_zip="${component_dir}/build/package-artifact-plugin.zip"
assert_zip_contains "$included_zip" "package-artifact-plugin/runtime/packages/static-site-importer.zip"
assert_zip_contains "$included_zip" "package-artifact-plugin/runtime/nested/packages/nested.zip"
assert_zip_not_contains "$included_zip" "package-artifact-plugin/accidental.zip"
assert_zip_not_contains "$included_zip" "package-artifact-plugin/runtime/omit/source.txt"
assert_zip_not_contains "$included_zip" "package-artifact-plugin/runtime/packages/linked.zip"

if ! grep -Fq '"type":"wordpress.package_artifacts"' "${TMP_DIR}/included-build.out"; then
    echo "Expected structured package artifact output" >&2
    sed 's/^/  /' "${TMP_DIR}/included-build.out" >&2
    exit 1
fi

if ! grep -Fq '"path":"runtime/packages/static-site-importer.zip"' "${TMP_DIR}/included-build.out"; then
    echo "Expected included artifact path in structured output" >&2
    sed 's/^/  /' "${TMP_DIR}/included-build.out" >&2
    exit 1
fi

if ! grep -Eq '"sha256":"[0-9a-f]{64}"' "${TMP_DIR}/included-build.out"; then
    echo "Expected included artifact SHA-256 in structured output" >&2
    sed 's/^/  /' "${TMP_DIR}/included-build.out" >&2
    exit 1
fi

if [ "$(grep -Fc '"path":"runtime/packages/static-site-importer.zip"' "${TMP_DIR}/included-build.out")" -ne 1 ]; then
    echo "Expected duplicate artifact patterns to emit one manifest entry" >&2
    sed 's/^/  /' "${TMP_DIR}/included-build.out" >&2
    exit 1
fi

mkdir -p "${component_dir}/.homeboy-build/stale/packages"
printf '%s\n' 'stale staging zip' > "${component_dir}/.homeboy-build/stale/packages/stale.zip"
(
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_artifacts":["**/*.zip"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/staging-build.out"
)

staging_zip="${component_dir}/build/package-artifact-plugin.zip"
assert_zip_not_contains "$staging_zip" "package-artifact-plugin/.homeboy-build/stale/packages/stale.zip"

if (
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_artifacts":["runtime/missing/*.zip"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/missing-build.out" 2>&1
); then
    echo "Expected missing declared package artifact pattern to fail" >&2
    sed 's/^/  /' "${TMP_DIR}/missing-build.out" >&2
    exit 1
fi

if ! grep -Fq 'Declared WordPress package artifact pattern matched no files: runtime/missing/*.zip' "${TMP_DIR}/missing-build.out"; then
    echo "Expected missing artifact error" >&2
    sed 's/^/  /' "${TMP_DIR}/missing-build.out" >&2
    exit 1
fi

if (
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_artifacts":["../outside/*.zip"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/traversal-build.out" 2>&1
); then
    echo "Expected traversal artifact pattern to fail" >&2
    exit 1
fi

if ! grep -Fq "cannot contain '..'" "${TMP_DIR}/traversal-build.out"; then
    echo "Expected traversal artifact error" >&2
    sed 's/^/  /' "${TMP_DIR}/traversal-build.out" >&2
    exit 1
fi

if (
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_artifacts":["/tmp/*.zip"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/absolute-build.out" 2>&1
); then
    echo "Expected absolute artifact pattern to fail" >&2
    exit 1
fi

if ! grep -Fq 'must be component-relative' "${TMP_DIR}/absolute-build.out"; then
    echo "Expected absolute artifact error" >&2
    sed 's/^/  /' "${TMP_DIR}/absolute-build.out" >&2
    exit 1
fi

if (
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_excludes":["../outside/"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/exclude-traversal-build.out" 2>&1
); then
    echo "Expected traversal package exclude to fail" >&2
    exit 1
fi

if ! grep -Fq 'Package excludes cannot contain traversal' "${TMP_DIR}/exclude-traversal-build.out"; then
    echo "Expected traversal package exclude error" >&2
    sed 's/^/  /' "${TMP_DIR}/exclude-traversal-build.out" >&2
    exit 1
fi

if (
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_excludes":[42]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/exclude-type-build.out" 2>&1
); then
    echo "Expected non-string package exclude to fail" >&2
    exit 1
fi

if ! grep -Fq 'package_excludes entries must be non-empty strings' "${TMP_DIR}/exclude-type-build.out"; then
    echo "Expected package exclude type error" >&2
    sed 's/^/  /' "${TMP_DIR}/exclude-type-build.out" >&2
    exit 1
fi

profile_dir="${TMP_DIR}/profile-plugin"
mkdir -p "${profile_dir}/includes" "${profile_dir}/docs" "${profile_dir}/.homeboy-build/stale"

cat > "${profile_dir}/profile-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Profile Plugin
 * Version: 1.0.0
 */
PHP

printf '%s\n' '<?php' > "${profile_dir}/includes/bootstrap.php"
printf '%s\n' '<?php' > "${profile_dir}/includes/keep.php"
printf '%s\n' 'unselected docs' > "${profile_dir}/docs/guide.md"
printf '%s\n' 'stale staging' > "${profile_dir}/.homeboy-build/stale/skip.txt"

cat > "${profile_dir}/package-manifest.json" <<'JSON'
{
  "profiles": {
    "runtime": {
      "selectors": [
        { "type": "file", "path": "profile-plugin.php" },
        { "type": "prefix", "path": "includes/" },
        { "type": "file", "path": "package-manifest.json" }
      ],
      "required_files": [
        "profile-plugin.php",
        "includes/bootstrap.php",
        "package-manifest.json"
      ]
    }
  }
}
JSON

(
    cd "$profile_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="profile-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_profile":{"manifest":"package-manifest.json","profile":"runtime"}}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/profile-build.out"
)

profile_zip="${profile_dir}/build/profile-plugin.zip"
assert_zip_contains "$profile_zip" "profile-plugin/profile-plugin.php"
assert_zip_contains "$profile_zip" "profile-plugin/includes/bootstrap.php"
assert_zip_contains "$profile_zip" "profile-plugin/includes/keep.php"
assert_zip_contains "$profile_zip" "profile-plugin/package-manifest.json"
assert_zip_not_contains "$profile_zip" "profile-plugin/docs/guide.md"
assert_zip_not_contains "$profile_zip" "profile-plugin/.homeboy-build/stale/skip.txt"

if ! grep -Fq '"type":"wordpress.package_profile"' "${TMP_DIR}/profile-build.out"; then
    echo "Expected structured package profile output" >&2
    sed 's/^/  /' "${TMP_DIR}/profile-build.out" >&2
    exit 1
fi

if ! grep -Fq '"manifest":"package-manifest.json"' "${TMP_DIR}/profile-build.out"; then
    echo "Expected package profile manifest identity" >&2
    sed 's/^/  /' "${TMP_DIR}/profile-build.out" >&2
    exit 1
fi

if ! grep -Fq '"profile":"runtime"' "${TMP_DIR}/profile-build.out"; then
    echo "Expected package profile name in structured output" >&2
    sed 's/^/  /' "${TMP_DIR}/profile-build.out" >&2
    exit 1
fi

if ! grep -Fq '"files":["includes/bootstrap.php","includes/keep.php","package-manifest.json","profile-plugin.php"]' "${TMP_DIR}/profile-build.out"; then
    echo "Expected deterministic selected-file inventory" >&2
    sed 's/^/  /' "${TMP_DIR}/profile-build.out" >&2
    exit 1
fi

(
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_profile":{}}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/empty-profile-build.out"
)

empty_profile_zip="${component_dir}/build/package-artifact-plugin.zip"
assert_zip_contains "$empty_profile_zip" "package-artifact-plugin/package-artifact-plugin.php"
assert_zip_contains "$empty_profile_zip" "package-artifact-plugin/includes/bootstrap.php"
assert_zip_not_contains "$empty_profile_zip" "package-artifact-plugin/accidental.zip"

if grep -Fq '"type":"wordpress.package_profile"' "${TMP_DIR}/empty-profile-build.out"; then
    echo "Empty package_profile must keep default rsync staging" >&2
    sed 's/^/  /' "${TMP_DIR}/empty-profile-build.out" >&2
    exit 1
fi

vendor_dir="${TMP_DIR}/vendor-profile-plugin"
mkdir -p "$vendor_dir"
cat > "${vendor_dir}/vendor-profile-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Vendor Profile Plugin
 * Version: 1.0.0
 */
PHP

cat > "${vendor_dir}/composer.json" <<'JSON'
{
  "name": "smoke/vendor-profile-plugin"
}
JSON

cat > "${vendor_dir}/package-manifest.json" <<'JSON'
{
  "profiles": {
    "runtime": {
      "selectors": [
        { "type": "file", "path": "vendor-profile-plugin.php" },
        { "type": "file", "path": "vendor/autoload.php" },
        { "type": "prefix", "path": "vendor/selected/" }
      ],
      "required_files": [
        "vendor-profile-plugin.php",
        "vendor/autoload.php"
      ]
    }
  }
}
JSON

fake_bin="${TMP_DIR}/bin"
mkdir -p "$fake_bin"
cat > "${fake_bin}/composer" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = "install" ]; then
    rm -rf vendor
    mkdir -p vendor/composer vendor/selected vendor/omitted
    printf '%s\n' '<?php' > vendor/autoload.php
    printf '%s\n' '<?php' > vendor/composer/autoload_real.php
    printf '%s\n' '<?php' > vendor/selected/lib.php
    printf '%s\n' '<?php' > vendor/omitted/secret.php
fi
exit 0
SH
chmod +x "${fake_bin}/composer"

(
    cd "$vendor_dir"
    PATH="${fake_bin}:$PATH" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="vendor-profile-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_profile":{"manifest":"package-manifest.json","profile":"runtime"}}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/vendor-profile-build.out"
)

vendor_zip="${vendor_dir}/build/vendor-profile-plugin.zip"
assert_zip_contains "$vendor_zip" "vendor-profile-plugin/vendor/autoload.php"
assert_zip_contains "$vendor_zip" "vendor-profile-plugin/vendor/selected/lib.php"
assert_zip_not_contains "$vendor_zip" "vendor-profile-plugin/vendor/omitted/secret.php"
assert_zip_not_contains "$vendor_zip" "vendor-profile-plugin/vendor/composer/autoload_real.php"

cat > "${profile_dir}/required-missing.json" <<'JSON'
{
  "profiles": {
    "runtime": {
      "selectors": [
        { "type": "file", "path": "profile-plugin.php" }
      ],
      "required_files": [
        "includes/missing.php"
      ]
    }
  }
}
JSON

cat > "${profile_dir}/missing-file.json" <<'JSON'
{
  "profiles": {
    "runtime": {
      "selectors": [
        { "type": "file", "path": "profile-plugin.php" },
        { "type": "file", "path": "includes/missing.php" }
      ]
    }
  }
}
JSON

cat > "${profile_dir}/traversal.json" <<'JSON'
{
  "profiles": {
    "runtime": {
      "selectors": [
        { "type": "file", "path": "../outside.php" }
      ]
    }
  }
}
JSON

cat > "${profile_dir}/absolute.json" <<'JSON'
{
  "profiles": {
    "runtime": {
      "selectors": [
        { "type": "file", "path": "/tmp/outside.php" }
      ]
    }
  }
}
JSON

cat > "${profile_dir}/malformed.json" <<'JSON'
{
  "profiles": {
    "runtime": {
      "selectors": [
        { "type": "glob", "path": "includes/" }
      ]
    }
  }
}
JSON

expect_profile_failure() {
    local out_file="$1"
    local settings="$2"
    local expected="$3"
    local message="$4"

    if (
        cd "$profile_dir"
        HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
        HOMEBOY_COMPONENT_ID="profile-plugin" \
        HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
        HOMEBOY_SKIP_TESTS=1 \
        HOMEBOY_SETTINGS_JSON="$settings" \
            bash "${EXTENSION_DIR}/scripts/build/build.sh" > "$out_file" 2>&1
    ); then
        echo "$message" >&2
        sed 's/^/  /' "$out_file" >&2
        exit 1
    fi

    if ! grep -Fq "$expected" "$out_file"; then
        echo "Expected error: $expected" >&2
        sed 's/^/  /' "$out_file" >&2
        exit 1
    fi
}

expect_profile_failure \
    "${TMP_DIR}/required-build.out" \
    '{"package_profile":{"manifest":"required-missing.json","profile":"runtime"}}' \
    'Package profile required file is missing: includes/missing.php' \
    'Expected missing required file to fail'

expect_profile_failure \
    "${TMP_DIR}/missing-file-build.out" \
    '{"package_profile":{"manifest":"missing-file.json","profile":"runtime"}}' \
    'Package profile file selector did not match a regular file: includes/missing.php' \
    'Expected missing explicit file selector to fail'

expect_profile_failure \
    "${TMP_DIR}/profile-traversal-build.out" \
    '{"package_profile":{"manifest":"traversal.json","profile":"runtime"}}' \
    'cannot contain traversal or filesystem-absolute paths' \
    'Expected traversal selector path to fail'

expect_profile_failure \
    "${TMP_DIR}/profile-absolute-build.out" \
    '{"package_profile":{"manifest":"absolute.json","profile":"runtime"}}' \
    'cannot contain traversal or filesystem-absolute paths' \
    'Expected absolute selector path to fail'

expect_profile_failure \
    "${TMP_DIR}/malformed-build.out" \
    '{"package_profile":{"manifest":"malformed.json","profile":"runtime"}}' \
    'Package profile selector type must be file or prefix' \
    'Expected malformed selector type to fail'

expect_profile_failure \
    "${TMP_DIR}/manifest-traversal-build.out" \
    '{"package_profile":{"manifest":"../outside.json","profile":"runtime"}}' \
    'cannot contain traversal or filesystem-absolute paths' \
    'Expected traversal manifest path to fail'

expect_profile_failure \
    "${TMP_DIR}/manifest-absolute-build.out" \
    '{"package_profile":{"manifest":"/tmp/outside.json","profile":"runtime"}}' \
    'cannot contain traversal or filesystem-absolute paths' \
    'Expected absolute manifest path to fail'

expect_profile_failure \
    "${TMP_DIR}/profile-name-build.out" \
    '{"package_profile":{"manifest":"package-manifest.json","profile":"../runtime"}}' \
    'extensions.wordpress.package_profile.profile must be a safe profile name' \
    'Expected unsafe profile name to fail'

echo "WordPress package artifacts build smoke passed."

