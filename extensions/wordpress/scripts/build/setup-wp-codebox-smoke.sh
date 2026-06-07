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
ARTIFACT_ROOT="${TMPDIR}/artifact-root"
ARTIFACT_PATH="${TMPDIR}/wp-codebox-cli-linux-x64.tar.gz"

mkdir -p "${FAKE_BIN}" "${HOME_DIR}" "${EXTENSION_DIR}" "${ARTIFACT_ROOT}/wp-codebox-cli/bin" "${ARTIFACT_ROOT}/wp-codebox-cli/packages/runtime-core/dist"

cat > "${ARTIFACT_ROOT}/wp-codebox-cli/bin/wp-codebox" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' 'wp-codebox release stub'
SH
chmod +x "${ARTIFACT_ROOT}/wp-codebox-cli/bin/wp-codebox"
printf '%s\n' 'export const fixture = true;' > "${ARTIFACT_ROOT}/wp-codebox-cli/packages/runtime-core/dist/index.js"
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

case "$1" in
    clone)
        dest="${@: -1}"
        mkdir -p "${dest}/.git"
        ;;
    -C)
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
        install)
            if [[ " $* " != *" --omit=optional "* ]]; then
                printf 'expected wp-codebox source install to omit optional dependencies: %s\n' "$*" >&2
                exit 1
            fi
            exit 0
            ;;
        run)
            mkdir -p "${prefix}/packages/cli/dist" "${prefix}/packages/runtime-core/dist"
            printf '%s\n' 'console.log("wp-codebox source stub")' > "${prefix}/packages/cli/dist/index.js"
            printf '%s\n' 'export const fixture = true;' > "${prefix}/packages/runtime-core/dist/index.js"
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

if [ "$("${source_wp_codebox_bin}")" != "wp-codebox source stub" ]; then
    echo "Expected source fallback wrapper to execute built CLI" >&2
    exit 1
fi

if [ ! -f "${source_wp_codebox_core_module}" ]; then
    echo "Expected source fallback to export built runtime core module" >&2
    exit 1
fi

# Cold-cache regression: a later CI step picks up a cached wp-codebox CLI via
# HOMEBOY_WP_CODEBOX_BIN, but HOMEBOY_WP_CODEBOX_CORE_MODULE did not survive the
# step/process boundary. The runtime-core module is still on disk from the
# earlier install. Setup must re-derive the core module from the known install
# locations instead of leaving it unset (which made the recipe loader hard-fail
# with "@automattic/wp-codebox-core not found" and "No tests ran"). The CLI bin
# is already present, so this path must NOT trigger a network install.
COLD_HOME="${TMPDIR}/cold-home"
COLD_INSTALL_DIR="${TMPDIR}/cold-install"
COLD_GITHUB_ENV_FILE="${TMPDIR}/cold-github-env"
COLD_BIN="${COLD_HOME}/.local/bin/wp-codebox"

mkdir -p \
    "${COLD_HOME}/.local/bin" \
    "${COLD_INSTALL_DIR}/source/packages/runtime-core/dist"

cat > "${COLD_BIN}" <<'SH'
#!/usr/bin/env bash
printf '%s\n' 'wp-codebox cached stub'
SH
chmod +x "${COLD_BIN}"
printf '%s\n' 'export const fixture = true;' > "${COLD_INSTALL_DIR}/source/packages/runtime-core/dist/index.js"

# A network would be a hard failure here: the bin already exists, so no curl/git
# install should run. Point curl/git at stubs that fail loudly if invoked.
cat > "${FAKE_BIN}/curl-fail" <<'SH'
#!/usr/bin/env bash
printf 'cold-cache path must not download via curl: %s\n' "$*" >&2
exit 1
SH
chmod +x "${FAKE_BIN}/curl-fail"

(
    cd "${EXTENSION_DIR}"
    HOME="${COLD_HOME}" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${COLD_GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_BIN="${COLD_BIN}" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${COLD_INSTALL_DIR}" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/cold-setup.out"
)

if ! grep -q '^HOMEBOY_WP_CODEBOX_CORE_MODULE=' "${COLD_GITHUB_ENV_FILE}"; then
    echo "Expected cold-cache setup to re-derive and export HOMEBOY_WP_CODEBOX_CORE_MODULE" >&2
    cat "${TMPDIR}/cold-setup.out" >&2
    exit 1
fi

cold_core_module="$(grep '^HOMEBOY_WP_CODEBOX_CORE_MODULE=' "${COLD_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
if [ "${cold_core_module}" != "${COLD_INSTALL_DIR}/source/packages/runtime-core/dist/index.js" ]; then
    echo "Expected cold-cache setup to resolve the on-disk runtime core module, got: ${cold_core_module}" >&2
    exit 1
fi

if grep -qi 'Installing WP Codebox CLI' "${TMPDIR}/cold-setup.out"; then
    echo "Cold-cache path must not reinstall the CLI when a cached bin and module exist" >&2
    cat "${TMPDIR}/cold-setup.out" >&2
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

if [ "$("${missing_release_wp_codebox_bin}")" != "wp-codebox source stub" ]; then
    echo "Expected missing release artifact to fall back to built CLI" >&2
    exit 1
fi

if ! grep -q 'WP Codebox release artifact not published' "${TMPDIR}/missing-release-setup.err"; then
    echo "Expected missing release artifact message" >&2
    exit 1
fi

echo "WP Codebox setup smoke passed"
