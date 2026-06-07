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

echo "WP Codebox setup smoke passed"
