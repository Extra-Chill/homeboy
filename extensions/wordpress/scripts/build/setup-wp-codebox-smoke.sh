#!/usr/bin/env bash
set -euo pipefail

TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NODE_BIN_DIR="$(dirname "$(command -v node)")"
FAKE_BIN="${TMPDIR}/bin"
HOME_DIR="${TMPDIR}/home"
EXTENSION_DIR="${TMPDIR}/extension"
GITHUB_ENV_FILE="${TMPDIR}/github-env"
SOURCE_GITHUB_ENV_FILE="${TMPDIR}/source-github-env"
MISSING_RELEASE_GITHUB_ENV_FILE="${TMPDIR}/missing-release-github-env"
OVERRIDE_GITHUB_ENV_FILE="${TMPDIR}/override-github-env"
ARTIFACT_ROOT="${TMPDIR}/artifact-root"
ARTIFACT_PATH="${TMPDIR}/wp-codebox-cli-linux-x64.tar.gz"

mkdir -p "${FAKE_BIN}" "${HOME_DIR}" "${EXTENSION_DIR}/scripts/build" "${ARTIFACT_ROOT}/wp-codebox-cli/bin" "${ARTIFACT_ROOT}/wp-codebox-cli/node_modules/@automattic/wp-codebox-core/dist" "${ARTIFACT_ROOT}/wp-codebox-cli/node_modules/sharp"

cat > "${EXTENSION_DIR}/scripts/build/persist-wp-codebox-overrides.mjs" <<'NODE'
#!/usr/bin/env node
process.exit(0);
NODE

cat > "${ARTIFACT_ROOT}/wp-codebox-cli/bin/wp-codebox" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' 'wp-codebox release stub'
SH
chmod +x "${ARTIFACT_ROOT}/wp-codebox-cli/bin/wp-codebox"
printf '%s\n' 'module.exports = { fixture: true };' > "${ARTIFACT_ROOT}/wp-codebox-cli/node_modules/@automattic/wp-codebox-core/dist/index.js"
printf '%s\n' 'module.exports = { runtimeContractManifest() { return {}; } };' > "${ARTIFACT_ROOT}/wp-codebox-cli/node_modules/@automattic/wp-codebox-core/dist/contracts.js"
printf '%s\n' 'module.exports = require("./native.js");' > "${ARTIFACT_ROOT}/wp-codebox-cli/node_modules/sharp/index.js"
printf '%s\n' 'module.exports = { native: true };' > "${ARTIFACT_ROOT}/wp-codebox-cli/node_modules/sharp/native.js"
tar -czf "${ARTIFACT_PATH}" -C "${ARTIFACT_ROOT}" wp-codebox-cli

cat > "${FAKE_BIN}/curl" <<SH
#!/usr/bin/env bash
set -euo pipefail

if [ "\${FAKE_WP_CODEBOX_RELEASE_MISSING:-}" = "1" ]; then
    if [ "\$#" -ge 2 ] && [ "\${1}" = "-fsIL" ]; then
        exit 22
    fi

    printf 'unexpected download after missing release probe: %s\n' "\$*" >&2
    exit 1
fi

if [ "\$#" -ge 2 ] && [ "\${1}" = "-fsIL" ]; then
    exit 0
fi

if [ "\$#" -lt 4 ] || [ "\${3}" != "-o" ]; then
    printf 'unexpected curl invocation: %s\n' "\$*" >&2
    exit 1
fi

cp "${ARTIFACT_PATH}" "\${4}"
SH
chmod +x "${FAKE_BIN}/curl"

cat > "${FAKE_BIN}/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

SOURCE_SHA="0123456789abcdef0123456789abcdef01234567"

case "$1" in
    clone)
        dest="${@: -1}"
        mkdir -p "${dest}/.git"
        if [ "${FAKE_WP_CODEBOX_NO_LOCKFILE:-}" != "1" ]; then
            printf '%s\n' '{}' > "${dest}/package-lock.json"
        fi
        ;;
    -C)
        if [ "${3:-}" = "rev-parse" ] && [ "${4:-}" = "HEAD" ]; then
            printf '%s\n' "${SOURCE_SHA}"
        fi
        exit 0
        ;;
    *)
        printf 'unexpected git invocation: %s\n' "$*" >&2
        exit 1
        ;;
esac
SH
chmod +x "${FAKE_BIN}/git"

cat > "${FAKE_BIN}/npm" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

prefix=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            prefix="$2"
            shift 2
            ;;
        ci)
            if [[ " $* " != *" --include=optional "* ]]; then
                printf 'expected wp-codebox source install to include optional dependencies: %s\n' "$*" >&2
                exit 1
            fi
            if [[ " $* " == *" --omit=optional "* ]]; then
                printf 'wp-codebox source install must not omit optional dependencies: %s\n' "$*" >&2
                exit 1
            fi
            mkdir -p "${prefix}/node_modules/sharp"
            printf '%s\n' 'module.exports = require("./native.js");' > "${prefix}/node_modules/sharp/index.js"
            printf '%s\n' 'module.exports = { native: true };' > "${prefix}/node_modules/sharp/native.js"
            exit 0
            ;;
        run)
            mkdir -p "${prefix}/node_modules/@automattic/wp-codebox-core/dist"
            mkdir -p "${prefix}/packages/cli/dist"
            printf '%s\n' 'module.exports = { fixture: true };' > "${prefix}/node_modules/@automattic/wp-codebox-core/dist/index.js"
            printf '%s\n' 'module.exports = { runtimeContractManifest() { return {}; } };' > "${prefix}/node_modules/@automattic/wp-codebox-core/dist/contracts.js"
            cat > "${prefix}/packages/cli/dist/index.js" <<'NODE'
#!/usr/bin/env node
console.log('wp-codebox source stub');
NODE
            chmod +x "${prefix}/packages/cli/dist/index.js"
            exit 0
            ;;
        *)
            shift
            ;;
    esac
done
SH
chmod +x "${FAKE_BIN}/npm"

(
    cd "${EXTENSION_DIR}"
    HOME="${HOME_DIR}" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_DOWNLOAD_URL="https://example.test/wp-codebox-cli-linux-x64.tar.gz" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/setup.out"
)

if ! grep -q '^HOMEBOY_WP_CODEBOX_BIN=' "${GITHUB_ENV_FILE}"; then
    echo "Expected setup to export HOMEBOY_WP_CODEBOX_BIN" >&2
    exit 1
fi

if ! grep -q '^HOMEBOY_WP_CODEBOX_CORE_MODULE=' "${GITHUB_ENV_FILE}"; then
    echo "Expected setup to export HOMEBOY_WP_CODEBOX_CORE_MODULE" >&2
    exit 1
fi

wp_codebox_bin="$(grep '^HOMEBOY_WP_CODEBOX_BIN=' "${GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"

if [ ! -x "${wp_codebox_bin}" ]; then
    echo "Expected wp-codebox wrapper to be executable" >&2
    exit 1
fi

if [ "$("${wp_codebox_bin}")" != "wp-codebox release stub" ]; then
    echo "Expected wp-codebox wrapper to execute release artifact CLI" >&2
    exit 1
fi

(
    cd "${EXTENSION_DIR}"
    HOME="${HOME_DIR}" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${SOURCE_GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_INSTALL_MODE="source" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/source-install" \
    HOMEBOY_WP_CODEBOX_SOURCE="https://example.test/wp-codebox.git" \
    HOMEBOY_WP_CODEBOX_REF="main" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/source-setup.out"
)

source_wp_codebox_bin="$(grep '^HOMEBOY_WP_CODEBOX_BIN=' "${SOURCE_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
source_wp_codebox_core_module="$(grep '^HOMEBOY_WP_CODEBOX_CORE_MODULE=' "${SOURCE_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"

if [ "$(PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" "${source_wp_codebox_bin}")" != "wp-codebox source stub" ]; then
    echo "Expected source fallback wrapper to execute built CLI" >&2
    exit 1
fi

if [ "${source_wp_codebox_bin}" != "${TMPDIR}/source-install/source/packages/cli/dist/index.js" ]; then
    echo "Expected source fallback to export the built WP Codebox CLI, got: ${source_wp_codebox_bin}" >&2
    exit 1
fi

if [ ! -f "${source_wp_codebox_core_module}" ]; then
    echo "Expected source fallback to export built runtime core module" >&2
    exit 1
fi

if ! grep -q '^WP_CODEBOX_SOURCE_REF=main$' "${SOURCE_GITHUB_ENV_FILE}"; then
    echo "Expected source fallback to export the requested WP Codebox ref" >&2
    cat "${SOURCE_GITHUB_ENV_FILE}" >&2
    exit 1
fi

if ! grep -q '^WP_CODEBOX_SOURCE_SHA=0123456789abcdef0123456789abcdef01234567$' "${SOURCE_GITHUB_ENV_FILE}"; then
    echo "Expected source fallback to export the resolved WP Codebox SHA" >&2
    cat "${SOURCE_GITHUB_ENV_FILE}" >&2
    exit 1
fi

# Cached native-runtime regression: `commands` can succeed while a lazy native
# dependency is unavailable. Setup must probe sharp and rebuild from the source
# lockfile instead of trusting the cached node_modules tree.
COLD_HOME="${TMPDIR}/cold-home"
COLD_INSTALL_DIR="${TMPDIR}/cold-install"
COLD_GITHUB_ENV_FILE="${TMPDIR}/cold-github-env"
COLD_BIN="${COLD_HOME}/.local/bin/wp-codebox"

mkdir -p \
    "${COLD_HOME}/.local/bin" \
    "${COLD_INSTALL_DIR}/source/node_modules/@automattic/wp-codebox-core/dist"

cat > "${COLD_BIN}" <<'SH'
#!/usr/bin/env bash
printf '%s\n' 'wp-codebox cached stub'
SH
chmod +x "${COLD_BIN}"
printf '%s\n' 'module.exports = { fixture: true };' > "${COLD_INSTALL_DIR}/source/node_modules/@automattic/wp-codebox-core/dist/index.js"
printf '%s\n' 'module.exports = { runtimeContractManifest() { return {}; } };' > "${COLD_INSTALL_DIR}/source/node_modules/@automattic/wp-codebox-core/dist/contracts.js"
mkdir -p "${COLD_INSTALL_DIR}/source/node_modules/sharp"
printf '%s\n' 'module.exports = require("./native.js");' > "${COLD_INSTALL_DIR}/source/node_modules/sharp/index.js"

(
    cd "${EXTENSION_DIR}"
    HOME="${COLD_HOME}" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${COLD_GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_BIN="${COLD_BIN}" \
    HOMEBOY_WP_CODEBOX_CORE_MODULE="${COLD_INSTALL_DIR}/source/packages/runtime-core/dist/index.js" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${COLD_INSTALL_DIR}" \
    FAKE_WP_CODEBOX_RELEASE_MISSING="1" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/cold-setup.out"
)

if ! grep -q '^HOMEBOY_WP_CODEBOX_CORE_MODULE=' "${COLD_GITHUB_ENV_FILE}"; then
    echo "Expected cold-cache setup to re-derive and export HOMEBOY_WP_CODEBOX_CORE_MODULE" >&2
    cat "${TMPDIR}/cold-setup.out" >&2
    exit 1
fi

cold_core_module="$(grep '^HOMEBOY_WP_CODEBOX_CORE_MODULE=' "${COLD_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
if [ "${cold_core_module}" != "${COLD_INSTALL_DIR}/source/node_modules/@automattic/wp-codebox-core/dist/contracts.js" ]; then
    echo "Expected cold-cache setup to rebuild and export the runtime core module, got: ${cold_core_module}" >&2
    exit 1
fi

if ! grep -q 'Installing WP Codebox CLI' "${TMPDIR}/cold-setup.out"; then
    echo "Broken cached runtime must be rebuilt instead of reused" >&2
    cat "${TMPDIR}/cold-setup.out" >&2
    exit 1
fi

if ! node -e 'require(require.resolve("sharp", { paths: [ process.argv[1] ] }));' "${COLD_INSTALL_DIR}/source"; then
    echo "Expected source hydration to restore the cached native runtime dependency" >&2
    exit 1
fi

EXTERNAL_CORE_ERROR="${TMPDIR}/external-core.err"
if (
    cd "${EXTENSION_DIR}"
    HOME="${COLD_HOME}" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${COLD_INSTALL_DIR}" \
    HOMEBOY_WP_CODEBOX_CORE_MODULE="${TMPDIR}/external/missing-core.js" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" >/dev/null 2> "${EXTERNAL_CORE_ERROR}"
); then
    echo "Missing external WP Codebox core overrides must fail closed" >&2
    exit 1
fi

if ! grep -q 'Explicit WP Codebox core module override is not a file' "${EXTERNAL_CORE_ERROR}"; then
    echo "Expected an explicit external core override diagnostic" >&2
    cat "${EXTERNAL_CORE_ERROR}" >&2
    exit 1
fi

NO_LOCKFILE_OUTPUT="${TMPDIR}/no-lockfile.out"
NO_LOCKFILE_ERROR="${TMPDIR}/no-lockfile.err"
if (
    cd "${EXTENSION_DIR}"
    HOME="${TMPDIR}/no-lockfile-home" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${TMPDIR}/no-lockfile-github-env" \
    HOMEBOY_WP_CODEBOX_INSTALL_MODE="source" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/no-lockfile-install" \
    HOMEBOY_WP_CODEBOX_SOURCE="https://example.test/custom-wp-codebox.git" \
    FAKE_WP_CODEBOX_NO_LOCKFILE="1" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${NO_LOCKFILE_OUTPUT}" 2> "${NO_LOCKFILE_ERROR}"
); then
    echo "Source setup without a lockfile must fail" >&2
    exit 1
fi

if ! grep -q 'WP Codebox source install requires an npm lockfile (package-lock.json or npm-shrinkwrap.json) for deterministic npm ci: https://example.test/custom-wp-codebox.git' "${NO_LOCKFILE_ERROR}"; then
    echo "Expected a deterministic lockfile diagnostic for custom WP Codebox sources" >&2
    cat "${NO_LOCKFILE_ERROR}" >&2
    exit 1
fi

STALE_ROOT="${TMPDIR}/stale-wp-codebox"
CURRENT_ROOT="${TMPDIR}/wp-codebox-main-current"
mkdir -p \
    "${STALE_ROOT}/packages/cli/dist" \
    "${STALE_ROOT}/packages/runtime-core/dist" \
    "${CURRENT_ROOT}/packages/cli/dist" \
    "${CURRENT_ROOT}/packages/runtime-core/dist" \
    "${CURRENT_ROOT}/node_modules/sharp"

cat > "${STALE_ROOT}/packages/cli/dist/index.js" <<'NODE'
#!/usr/bin/env node
console.log('stale wp-codebox');
NODE
chmod +x "${STALE_ROOT}/packages/cli/dist/index.js"
printf '%s\n' 'module.exports = { runtimeContractManifest() { return { fixture: "stale" }; } };' > "${STALE_ROOT}/packages/runtime-core/dist/index.js"

cat > "${CURRENT_ROOT}/packages/cli/dist/index.js" <<'NODE'
#!/usr/bin/env node
console.log('current wp-codebox');
NODE
chmod +x "${CURRENT_ROOT}/packages/cli/dist/index.js"
printf '%s\n' 'module.exports = { runtimeContractManifest() { return { fixture: "current" }; } };' > "${CURRENT_ROOT}/packages/runtime-core/dist/index.js"
printf '%s\n' 'module.exports = require("./native.js");' > "${CURRENT_ROOT}/node_modules/sharp/index.js"
printf '%s\n' 'module.exports = { native: true };' > "${CURRENT_ROOT}/node_modules/sharp/native.js"

(
    cd "${EXTENSION_DIR}"
    HOME="${HOME_DIR}" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${OVERRIDE_GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_BIN="${STALE_ROOT}/packages/cli/dist/index.js" \
    HOMEBOY_WP_CODEBOX_CORE_MODULE="${STALE_ROOT}/packages/runtime-core/dist/index.js" \
    WP_CODEBOX_CLI="${CURRENT_ROOT}/packages/cli/dist/index.js" \
    WP_CODEBOX_CORE_MODULE="${CURRENT_ROOT}/packages/runtime-core/dist/index.js" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/override-setup.out"
)

override_wp_codebox_bin="$(grep '^HOMEBOY_WP_CODEBOX_BIN=' "${OVERRIDE_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
override_wp_codebox_core_module="$(grep '^HOMEBOY_WP_CODEBOX_CORE_MODULE=' "${OVERRIDE_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"

if [ "${override_wp_codebox_bin}" != "${CURRENT_ROOT}/packages/cli/dist/index.js" ]; then
    echo "Expected explicit WP_CODEBOX_CLI to replace stale configured CLI, got: ${override_wp_codebox_bin}" >&2
    cat "${TMPDIR}/override-setup.out" >&2
    exit 1
fi

if [ "${override_wp_codebox_core_module}" != "${CURRENT_ROOT}/packages/runtime-core/dist/index.js" ]; then
    echo "Expected explicit WP_CODEBOX_CORE_MODULE to replace stale configured core module, got: ${override_wp_codebox_core_module}" >&2
    cat "${TMPDIR}/override-setup.out" >&2
    exit 1
fi

if grep -q "${STALE_ROOT}" "${OVERRIDE_GITHUB_ENV_FILE}"; then
    echo "Explicit override reinstall must not export stale WP Codebox paths" >&2
    cat "${OVERRIDE_GITHUB_ENV_FILE}" >&2
    exit 1
fi

SOURCE_PRECEDENCE_HOME="${TMPDIR}/source-precedence-home"
SOURCE_PRECEDENCE_INSTALL_DIR="${TMPDIR}/source-precedence-install"
SOURCE_PRECEDENCE_GITHUB_ENV_FILE="${TMPDIR}/source-precedence-github-env"
SOURCE_PRECEDENCE_STALE_BIN="${SOURCE_PRECEDENCE_HOME}/.local/bin/wp-codebox"

mkdir -p "${SOURCE_PRECEDENCE_HOME}/.local/bin"
cat > "${SOURCE_PRECEDENCE_STALE_BIN}" <<'SH'
#!/usr/bin/env bash
printf '%s\n' 'stale configured wp-codebox'
SH
chmod +x "${SOURCE_PRECEDENCE_STALE_BIN}"

(
    cd "${EXTENSION_DIR}"
    HOME="${SOURCE_PRECEDENCE_HOME}" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${SOURCE_PRECEDENCE_GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_BIN="${SOURCE_PRECEDENCE_STALE_BIN}" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${SOURCE_PRECEDENCE_INSTALL_DIR}" \
    HOMEBOY_WP_CODEBOX_SOURCE="https://example.test/wp-codebox.git" \
    HOMEBOY_WP_CODEBOX_REF="main" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/source-precedence-setup.out"
)

source_precedence_wp_codebox_bin="$(grep '^HOMEBOY_WP_CODEBOX_BIN=' "${SOURCE_PRECEDENCE_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"

if [ "${source_precedence_wp_codebox_bin}" != "${SOURCE_PRECEDENCE_INSTALL_DIR}/source/packages/cli/dist/index.js" ]; then
    echo "Expected HOMEBOY_WP_CODEBOX_SOURCE to replace stale configured CLI, got: ${source_precedence_wp_codebox_bin}" >&2
    cat "${TMPDIR}/source-precedence-setup.out" >&2
    exit 1
fi

if [ "$(PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" "${source_precedence_wp_codebox_bin}")" != "wp-codebox source stub" ]; then
    echo "Expected source-precedence setup to execute built WP Codebox CLI" >&2
    exit 1
fi

(
    cd "${EXTENSION_DIR}"
    HOME="${HOME_DIR}" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${MISSING_RELEASE_GITHUB_ENV_FILE}" \
    FAKE_WP_CODEBOX_RELEASE_MISSING="1" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/missing-release-install" \
    HOMEBOY_WP_CODEBOX_DOWNLOAD_URL="https://example.test/wp-codebox-cli-linux-x64.tar.gz" \
    HOMEBOY_WP_CODEBOX_SOURCE="https://example.test/wp-codebox.git" \
    HOMEBOY_WP_CODEBOX_REF="main" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/missing-release-setup.out" 2> "${TMPDIR}/missing-release-setup.err"
)

missing_release_wp_codebox_bin="$(grep '^HOMEBOY_WP_CODEBOX_BIN=' "${MISSING_RELEASE_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"

if [ "$(PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" "${missing_release_wp_codebox_bin}")" != "wp-codebox source stub" ]; then
    echo "Expected missing release artifact to fall back to built CLI" >&2
    exit 1
fi

if [ "${missing_release_wp_codebox_bin}" != "${TMPDIR}/missing-release-install/source/packages/cli/dist/index.js" ]; then
    echo "Expected missing release fallback to export the built WP Codebox CLI, got: ${missing_release_wp_codebox_bin}" >&2
    exit 1
fi

if grep -q 'WP Codebox release artifact not published' "${TMPDIR}/missing-release-setup.err"; then
    echo "Explicit source/ref setup should not probe release artifacts before source install" >&2
    exit 1
fi

echo "WP Codebox setup smoke passed"
