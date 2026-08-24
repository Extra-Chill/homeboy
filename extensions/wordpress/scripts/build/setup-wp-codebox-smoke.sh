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
REAL_GIT_BIN="${TMPDIR}/real-git-bin"

mkdir -p "${FAKE_BIN}" "${REAL_GIT_BIN}" "${HOME_DIR}" "${EXTENSION_DIR}/scripts/build" "${ARTIFACT_ROOT}/wp-codebox-cli/bin" "${ARTIFACT_ROOT}/wp-codebox-cli/node_modules/@automattic/wp-codebox-core/dist" "${ARTIFACT_ROOT}/wp-codebox-cli/node_modules/sharp"
ln -s "$(command -v git)" "${REAL_GIT_BIN}/git"

cat > "${EXTENSION_DIR}/scripts/build/persist-wp-codebox-overrides.mjs" <<'NODE'
#!/usr/bin/env node
process.exit(0);
NODE

cat > "${ARTIFACT_ROOT}/wp-codebox-cli/bin/wp-codebox" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
    printf '%s\n' "${FAKE_WP_CODEBOX_VERSION-0.21.0}"
    exit 0
fi
if [ "${1:-}" = "doctor" ] && [ "${2:-}" = "--json" ]; then
    version="${FAKE_WP_CODEBOX_VERSION-0.21.0}"
    printf '{"schema":"wp-codebox/doctor/v1","status":"ok","checks":[{"id":"wp-codebox.source","status":"ok","message":"packaged provenance verified","details":{"provenance":{"schema":"wp-codebox/cli-build-provenance/v1","package":{"name":"@automattic/wp-codebox-cli","version":"%s"},"dist":{"sha256":"release-dist"},"git":{}}}}]}\n' "${version}"
    exit 0
fi
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
            if [ -n "${FAKE_EXPECT_WP_CODEBOX_CLI+x}" ] && [ "${WP_CODEBOX_CLI:-}" != "${FAKE_EXPECT_WP_CODEBOX_CLI}" ]; then
                printf 'expected explicit external WP_CODEBOX_CLI to survive source fallback: %s\n' "${WP_CODEBOX_CLI:-}" >&2
                exit 1
            fi
            if [ -n "${FAKE_EXPECT_WP_CODEBOX_CORE_MODULE+x}" ] && [ "${WP_CODEBOX_CORE_MODULE:-}" != "${FAKE_EXPECT_WP_CODEBOX_CORE_MODULE}" ]; then
                printf 'expected explicit external WP_CODEBOX_CORE_MODULE to survive source fallback: %s\n' "${WP_CODEBOX_CORE_MODULE:-}" >&2
                exit 1
            fi
            mkdir -p "${prefix}/node_modules/@automattic/wp-codebox-core/dist"
            mkdir -p "${prefix}/packages/cli/dist"
            printf '%s\n' 'module.exports = { fixture: true };' > "${prefix}/node_modules/@automattic/wp-codebox-core/dist/index.js"
            printf '%s\n' 'module.exports = { runtimeContractManifest() { return {}; } };' > "${prefix}/node_modules/@automattic/wp-codebox-core/dist/contracts.js"
cat > "${prefix}/packages/cli/dist/index.js" <<'NODE'
#!/usr/bin/env node
if (process.argv.includes('--version')) { process.stdout.write(process.env.FAKE_WP_CODEBOX_SOURCE_VERSION ?? '0.21.0'); process.exit(0); }
if (process.argv.includes('doctor') && process.argv.includes('--json')) {
  const ref = process.env.WP_CODEBOX_SOURCE_REF ?? 'main';
  const commit = process.env.WP_CODEBOX_SOURCE_SHA ?? '0123456789abcdef0123456789abcdef01234567';
  process.stdout.write(JSON.stringify({ schema: 'wp-codebox/doctor/v1', status: 'ok', checks: [{ id: 'wp-codebox.source', status: 'ok', message: 'source provenance verified; no configured upstream is available and remote fetch was not attempted', details: { provenance: { schema: 'wp-codebox/cli-build-provenance/v1', package: { name: '@automattic/wp-codebox-cli', version: process.env.FAKE_WP_CODEBOX_SOURCE_VERSION ?? '0.21.0' }, dist: { sha256: 'source-dist' }, git: { ref, commit } }, git: { evidence: 'unavailable', reason: 'no configured upstream', remoteFetch: 'not-attempted' } } }] }) + '\n');
  process.exit(0);
}
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

# A runnable global workspace package must not shadow an already-current
# managed release, and the no-op path must not contact the release authority.
GLOBAL_BIN="${TMPDIR}/global-bin"
CURRENT_GITHUB_ENV_FILE="${TMPDIR}/current-github-env"
mkdir -p "${GLOBAL_BIN}"
cat > "${GLOBAL_BIN}/wp-codebox" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = "--version" ]; then printf '%s\n' '0.21.0'; exit 0; fi
if [ "${1:-}" = "doctor" ]; then
    printf '%s\n' '{"schema":"wp-codebox/doctor/v1","status":"ok","checks":[{"id":"wp-codebox.source","status":"ok","message":"stale packaged provenance","details":{"provenance":{"schema":"wp-codebox/cli-build-provenance/v1","package":{"name":"@automattic/wp-codebox-cli","version":"0.21.0"},"dist":{"sha256":"stale-global-dist"},"git":{}}}}]}'
    exit 0
fi
printf '%s\n' 'stale global wp-codebox'
SH
chmod +x "${GLOBAL_BIN}/wp-codebox"

(
    cd "${EXTENSION_DIR}"
    HOME="${HOME_DIR}" \
    PATH="${GLOBAL_BIN}:${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${CURRENT_GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_DOWNLOAD_URL="https://example.test/wp-codebox-cli-linux-x64.tar.gz" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/current-setup.out"
)

current_wp_codebox_bin="$(grep '^HOMEBOY_WP_CODEBOX_BIN=' "${CURRENT_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
if [ "${current_wp_codebox_bin}" != "${HOME_DIR}/.cache/homeboy/wp-codebox/release/wp-codebox-cli/bin/wp-codebox" ]; then
    echo "Expected current managed release to win over stale global CLI, got: ${current_wp_codebox_bin}" >&2
    cat "${TMPDIR}/current-setup.out" >&2
    exit 1
fi
if ! grep -q 'WP Codebox managed release is already current' "${TMPDIR}/current-setup.out" || grep -q 'Installing WP Codebox CLI' "${TMPDIR}/current-setup.out"; then
    echo "Expected an already-current managed release to be a no-op" >&2
    cat "${TMPDIR}/current-setup.out" >&2
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

# A prior setup may have left the managed source cache pointing at a removed or
# different repository. Source setup must replace that origin and its checked
# out ref with the requested source before building.
STALE_ORIGIN_INSTALL_DIR="${TMPDIR}/stale-origin-install"
STALE_ORIGIN_GITHUB_ENV_FILE="${TMPDIR}/stale-origin-github-env"
STALE_ORIGIN_REPO_DIR="${STALE_ORIGIN_INSTALL_DIR}/source"
REQUESTED_SOURCE="-requested-wp-codebox.git"
REQUESTED_SOURCE_PATH="${STALE_ORIGIN_REPO_DIR}/${REQUESTED_SOURCE}"
REQUESTED_WORKTREE="${TMPDIR}/requested-wp-codebox-worktree"
DIFFERENT_SOURCE="${TMPDIR}/different-wp-codebox.git"
DIFFERENT_WORKTREE="${TMPDIR}/different-wp-codebox-worktree"

git init --bare --quiet "${DIFFERENT_SOURCE}"
git clone --quiet "${DIFFERENT_SOURCE}" "${DIFFERENT_WORKTREE}"
git -C "${DIFFERENT_WORKTREE}" config user.email smoke@example.com
git -C "${DIFFERENT_WORKTREE}" config user.name Smoke
printf '%s\n' 'different source' > "${DIFFERENT_WORKTREE}/fixture.txt"
git -C "${DIFFERENT_WORKTREE}" add fixture.txt
git -C "${DIFFERENT_WORKTREE}" commit --quiet -m 'different source fixture'
DIFFERENT_SHA="$(git -C "${DIFFERENT_WORKTREE}" rev-parse HEAD)"
git -C "${DIFFERENT_WORKTREE}" push --quiet origin HEAD:main
mkdir -p "${STALE_ORIGIN_INSTALL_DIR}"
git clone --quiet --branch main "${DIFFERENT_SOURCE}" "${STALE_ORIGIN_REPO_DIR}"

if [ "$(git -C "${STALE_ORIGIN_REPO_DIR}" rev-parse HEAD)" != "${DIFFERENT_SHA}" ]; then
    echo "Expected stale cache fixture to start at the different source ref" >&2
    exit 1
fi

# The source is deliberately relative and begins with a dash. Its bare fixture
# lives in the managed checkout because `git -C "${STALE_ORIGIN_REPO_DIR}"`
# resolves relative remotes from that controlled working directory.
git init --bare --quiet "${REQUESTED_SOURCE_PATH}"
git clone --quiet "${REQUESTED_SOURCE_PATH}" "${REQUESTED_WORKTREE}"
git -C "${REQUESTED_WORKTREE}" config user.email smoke@example.com
git -C "${REQUESTED_WORKTREE}" config user.name Smoke
printf '%s\n' '{"scripts":{"build":"node -e 0"}}' > "${REQUESTED_WORKTREE}/package.json"
printf '%s\n' '{"lockfileVersion":3,"packages":{}}' > "${REQUESTED_WORKTREE}/package-lock.json"
git -C "${REQUESTED_WORKTREE}" add package.json package-lock.json
git -C "${REQUESTED_WORKTREE}" commit --quiet -m 'requested source fixture'
REQUESTED_SHA="$(git -C "${REQUESTED_WORKTREE}" rev-parse HEAD)"
git -C "${REQUESTED_WORKTREE}" push --quiet origin HEAD:main

# Newer Git versions accept `--` as an option separator in either position.
# Keep the fixture's positional contract deterministic while its successful
# setup run uses the real Git binary below.
git() {
    if [ "${1:-}" = "-C" ] && [ "${3:-}" = "remote" ] && [ "${4:-}" = "set-url" ] && [ "${5:-}" = "origin" ] && [ "${6:-}" = "--" ] && [ "${7:-}" = "${REQUESTED_SOURCE}" ]; then
        return 1
    fi
    command git "$@"
}

# `--` after the remote name is parsed as the replacement URL, not an option
# terminator. Keep this fixture dash-prefixed so the old ordering must fail.
if git -C "${STALE_ORIGIN_REPO_DIR}" remote set-url origin -- "${REQUESTED_SOURCE}"; then
    echo "Expected the previous git remote set-url delimiter ordering to fail" >&2
    exit 1
fi

if [ "$(git -C "${STALE_ORIGIN_REPO_DIR}" remote get-url origin)" != "${DIFFERENT_SOURCE}" ]; then
    echo "Expected failed delimiter ordering to leave the stale origin unchanged" >&2
    exit 1
fi

git -C "${STALE_ORIGIN_REPO_DIR}" remote set-url -- origin "${REQUESTED_SOURCE}"
if [ "$(git -C "${STALE_ORIGIN_REPO_DIR}" remote get-url origin)" != "${REQUESTED_SOURCE}" ]; then
    echo "Expected corrected git remote set-url delimiter ordering to replace the stale origin" >&2
    exit 1
fi

# Restore the stale cache so setup itself must perform the same repair.
git -C "${STALE_ORIGIN_REPO_DIR}" remote set-url -- origin "${DIFFERENT_SOURCE}"

# Git 2.50 rejects a local transport pathname beginning with `-`. Preserve the
# stored dash-prefixed relative source while rewriting only the transport path.
command git -C "${STALE_ORIGIN_REPO_DIR}" config "url../${REQUESTED_SOURCE}.insteadOf" "${REQUESTED_SOURCE}"

(
    cd "${EXTENSION_DIR}"
    HOME="${TMPDIR}/stale-origin-home" \
    PATH="${REAL_GIT_BIN}:${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${STALE_ORIGIN_GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_INSTALL_MODE="source" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${STALE_ORIGIN_INSTALL_DIR}" \
    HOMEBOY_WP_CODEBOX_SOURCE="${REQUESTED_SOURCE}" \
    HOMEBOY_WP_CODEBOX_REF="main" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/stale-origin-setup.out"
)

command git -C "${STALE_ORIGIN_REPO_DIR}" config --unset-all "url../${REQUESTED_SOURCE}.insteadOf"

stale_origin_wp_codebox_bin="$(grep '^HOMEBOY_WP_CODEBOX_BIN=' "${STALE_ORIGIN_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
stale_origin_wp_codebox_core_module="$(grep '^HOMEBOY_WP_CODEBOX_CORE_MODULE=' "${STALE_ORIGIN_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
if [ "${stale_origin_wp_codebox_bin}" != "${STALE_ORIGIN_REPO_DIR}/packages/cli/dist/index.js" ] || [ "$("${stale_origin_wp_codebox_bin}" --version)" != "0.21.0" ]; then
    echo "Expected stale-origin repair to install a minimum-compatible source CLI" >&2
    cat "${TMPDIR}/stale-origin-setup.out" >&2
    exit 1
fi

if [ "${stale_origin_wp_codebox_core_module}" != "${STALE_ORIGIN_REPO_DIR}/node_modules/@automattic/wp-codebox-core/dist/contracts.js" ] || [ ! -f "${stale_origin_wp_codebox_core_module}" ]; then
    echo "Expected stale-origin repair to install the source core module" >&2
    cat "${TMPDIR}/stale-origin-setup.out" >&2
    exit 1
fi

if [ "$(git -C "${STALE_ORIGIN_REPO_DIR}" remote get-url origin)" != "${REQUESTED_SOURCE}" ]; then
    echo "Expected stale managed cache origin to be replaced with requested local source" >&2
    exit 1
fi

if [ "$(git -C "${STALE_ORIGIN_REPO_DIR}" rev-parse HEAD)" != "${REQUESTED_SHA}" ]; then
    echo "Expected stale managed cache HEAD to converge to the requested source ref" >&2
    exit 1
fi

# An older release artifact is never accepted just because it responds to
# `commands`: setup must continue through its existing source provider path.
OLD_RELEASE_GITHUB_ENV_FILE="${TMPDIR}/old-release-github-env"
(
    cd "${EXTENSION_DIR}"
    HOME="${TMPDIR}/old-release-home" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${OLD_RELEASE_GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/old-release-install" \
    HOMEBOY_WP_CODEBOX_DOWNLOAD_URL="https://example.test/wp-codebox-cli-linux-x64.tar.gz" \
    FAKE_WP_CODEBOX_VERSION="0.20.1" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/old-release-setup.out" 2>&1
)

old_release_wp_codebox_bin="$(grep '^HOMEBOY_WP_CODEBOX_BIN=' "${OLD_RELEASE_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
old_release_wp_codebox_core_module="$(grep '^HOMEBOY_WP_CODEBOX_CORE_MODULE=' "${OLD_RELEASE_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
if [ "${old_release_wp_codebox_bin}" != "${TMPDIR}/old-release-install/source/packages/cli/dist/index.js" ]; then
    echo "Expected incompatible release runtime to be repaired through source setup, got: ${old_release_wp_codebox_bin}" >&2
    cat "${TMPDIR}/old-release-setup.out" >&2
    exit 1
fi

if [ "${old_release_wp_codebox_core_module}" != "${TMPDIR}/old-release-install/source/node_modules/@automattic/wp-codebox-core/dist/contracts.js" ]; then
    echo "Expected incompatible release fallback to export the source WP Codebox core module, got: ${old_release_wp_codebox_core_module}" >&2
    cat "${TMPDIR}/old-release-setup.out" >&2
    exit 1
fi

if ! grep -q 'wp_codebox_version_too_old' "${TMPDIR}/old-release-setup.out"; then
    echo "Expected incompatible release runtime to report the adapter version preflight" >&2
    cat "${TMPDIR}/old-release-setup.out" >&2
    exit 1
fi

# A runtime without a parseable version is equally unsafe to dispatch. Setup
# repairs the release through the configured source provider before tests run.
MISSING_VERSION_GITHUB_ENV_FILE="${TMPDIR}/missing-version-github-env"
(
    cd "${EXTENSION_DIR}"
    HOME="${TMPDIR}/missing-version-home" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${MISSING_VERSION_GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/missing-version-install" \
    HOMEBOY_WP_CODEBOX_DOWNLOAD_URL="https://example.test/wp-codebox-cli-linux-x64.tar.gz" \
    FAKE_WP_CODEBOX_VERSION="" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/missing-version-setup.out" 2>&1
)

missing_version_wp_codebox_bin="$(grep '^HOMEBOY_WP_CODEBOX_BIN=' "${MISSING_VERSION_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
if [ "${missing_version_wp_codebox_bin}" != "${TMPDIR}/missing-version-install/source/packages/cli/dist/index.js" ]; then
    echo "Expected release without a version to be repaired through source setup, got: ${missing_version_wp_codebox_bin}" >&2
    cat "${TMPDIR}/missing-version-setup.out" >&2
    exit 1
fi

if ! grep -q 'provenance_identity_incomplete' "${TMPDIR}/missing-version-setup.out"; then
    echo "Expected release without an immutable version identity to report the provenance rejection" >&2
    cat "${TMPDIR}/missing-version-setup.out" >&2
    exit 1
fi

# A configured source that cannot meet the manifest minimum must fail during
# setup with the same operator repair command used by the test adapter.
SOURCE_MINIMUM_ERROR="${TMPDIR}/source-minimum.err"
if (
    cd "${EXTENSION_DIR}"
    HOME="${TMPDIR}/source-minimum-home" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    HOMEBOY_WP_CODEBOX_INSTALL_MODE="source" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/source-minimum-install" \
    FAKE_WP_CODEBOX_SOURCE_VERSION="0.20.1" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" >/dev/null 2> "${SOURCE_MINIMUM_ERROR}"
); then
    echo "An incompatible configured source runtime must fail setup" >&2
    exit 1
fi

if ! grep -q 'WP Codebox wp_codebox_version_too_old: required >=0.21.0, observed 0.20.1' "${SOURCE_MINIMUM_ERROR}" || ! grep -q 'Run homeboy extension setup wordpress\.' "${SOURCE_MINIMUM_ERROR}"; then
    echo "Expected exact WP Codebox minimum-version repair command" >&2
    cat "${SOURCE_MINIMUM_ERROR}" >&2
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

if ! grep -q 'WP Codebox source install requires an npm lockfile (package-lock.json or npm-shrinkwrap.json) for deterministic npm ci from the configured source authority' "${NO_LOCKFILE_ERROR}"; then
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
if (process.argv.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (process.argv.includes('doctor') && process.argv.includes('--json')) {
  process.stdout.write(JSON.stringify({ schema: 'wp-codebox/doctor/v1', status: 'ok', checks: [{ id: 'wp-codebox.source', status: 'ok', message: 'packaged provenance verified', details: { provenance: { schema: 'wp-codebox/cli-build-provenance/v1', package: { name: '@automattic/wp-codebox-cli', version: '0.21.0' }, dist: { sha256: 'override-dist' }, git: {} } } }] }) + '\n');
  process.exit(0);
}
console.log('current wp-codebox');
NODE
chmod +x "${CURRENT_ROOT}/packages/cli/dist/index.js"
printf '%s\n' 'module.exports = { runtimeContractManifest() { return { fixture: "current" }; } };' > "${CURRENT_ROOT}/packages/runtime-core/dist/index.js"
printf '%s\n' 'module.exports = require("./native.js");' > "${CURRENT_ROOT}/node_modules/sharp/index.js"
printf '%s\n' 'module.exports = { native: true };' > "${CURRENT_ROOT}/node_modules/sharp/native.js"

OVERRIDE_INSTALL_DIR="${TMPDIR}/override-install"
OVERRIDE_DOWNLOAD_URL="https://example.test/override-wp-codebox-cli-linux-x64.tar.gz"
mkdir -p "${OVERRIDE_INSTALL_DIR}"
node -e 'const { createHash } = require("node:crypto"); const fs = require("node:fs"); fs.writeFileSync(process.argv[2], JSON.stringify({ schema: "homeboy-wordpress/wp-codebox-managed-release/v1", authority_sha256: createHash("sha256").update(process.argv[1]).digest("hex"), version: "0.21.0", dist_sha256: "override-dist" }));' "${OVERRIDE_DOWNLOAD_URL}" "${OVERRIDE_INSTALL_DIR}/managed-release-identity.json"

(
    cd "${EXTENSION_DIR}"
    HOME="${HOME_DIR}" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${OVERRIDE_GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${OVERRIDE_INSTALL_DIR}" \
    HOMEBOY_WP_CODEBOX_DOWNLOAD_URL="${OVERRIDE_DOWNLOAD_URL}" \
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

MISSING_RELEASE_INSTALL_DIR="${TMPDIR}/missing-release-install"
MISSING_RELEASE_RUNTIME_DIR="${MISSING_RELEASE_INSTALL_DIR}/release/wp-codebox-cli"
MISSING_RELEASE_HOME="${TMPDIR}/missing-release-home"
MISSING_RELEASE_WRAPPER="${MISSING_RELEASE_HOME}/.local/bin/wp-codebox"
mkdir -p "${MISSING_RELEASE_RUNTIME_DIR}/bin" "${MISSING_RELEASE_RUNTIME_DIR}/node_modules/@automattic/wp-codebox-core/dist"
cat > "${MISSING_RELEASE_RUNTIME_DIR}/bin/wp-codebox" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = "--version" ]; then
    printf '%s\n' '0.20.1'
    exit 0
fi
printf '%s\n' 'persisted release wp-codebox'
SH
chmod +x "${MISSING_RELEASE_RUNTIME_DIR}/bin/wp-codebox"
printf '%s\n' 'module.exports = { runtimeContractManifest() { return { fixture: "persisted-release" }; } };' > "${MISSING_RELEASE_RUNTIME_DIR}/node_modules/@automattic/wp-codebox-core/dist/contracts.js"
mkdir -p "$(dirname "${MISSING_RELEASE_WRAPPER}")"
cat > "${MISSING_RELEASE_WRAPPER}" <<EOF
#!/usr/bin/env bash
exec "${MISSING_RELEASE_RUNTIME_DIR}/bin/wp-codebox" "\$@"
EOF
chmod +x "${MISSING_RELEASE_WRAPPER}"

(
    cd "${EXTENSION_DIR}"
    HOME="${MISSING_RELEASE_HOME}" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${MISSING_RELEASE_GITHUB_ENV_FILE}" \
    FAKE_WP_CODEBOX_RELEASE_MISSING="1" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${MISSING_RELEASE_INSTALL_DIR}" \
    HOMEBOY_WP_CODEBOX_BIN="${MISSING_RELEASE_WRAPPER}" \
    HOMEBOY_WP_CODEBOX_CORE_MODULE="${MISSING_RELEASE_RUNTIME_DIR}/node_modules/@automattic/wp-codebox-core/dist/contracts.js" \
    HOMEBOY_WP_CODEBOX_DOWNLOAD_URL="https://example.test/wp-codebox-cli-linux-x64.tar.gz" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/missing-release-setup.out" 2> "${TMPDIR}/missing-release-setup.err"
)

missing_release_wp_codebox_bin="$(grep '^HOMEBOY_WP_CODEBOX_BIN=' "${MISSING_RELEASE_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
missing_release_wp_codebox_core_module="$(grep '^HOMEBOY_WP_CODEBOX_CORE_MODULE=' "${MISSING_RELEASE_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"

if [ "$(PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" "${missing_release_wp_codebox_bin}")" != "wp-codebox source stub" ]; then
    echo "Expected missing release artifact to fall back to built CLI" >&2
    exit 1
fi

if [ "${missing_release_wp_codebox_bin}" != "${MISSING_RELEASE_INSTALL_DIR}/source/packages/cli/dist/index.js" ]; then
    echo "Expected missing release fallback to export the built WP Codebox CLI, got: ${missing_release_wp_codebox_bin}" >&2
    exit 1
fi

if [ "${missing_release_wp_codebox_core_module}" != "${MISSING_RELEASE_INSTALL_DIR}/source/node_modules/@automattic/wp-codebox-core/dist/contracts.js" ]; then
    echo "Expected missing release fallback to export the source WP Codebox core module, got: ${missing_release_wp_codebox_core_module}" >&2
    cat "${TMPDIR}/missing-release-setup.out" >&2
    exit 1
fi

if ! grep -q 'WP Codebox release artifact is unavailable from the configured authority' "${TMPDIR}/missing-release-setup.err"; then
    echo "Missing release artifact must use the source fallback" >&2
    cat "${TMPDIR}/missing-release-setup.err" >&2
    exit 1
fi

# Source fallback must not erase caller-owned CLI/core values outside its
# managed release cache. This runs under setup's `set -e`; the rejected release
# must still converge the source cache after the final preserved override makes
# the wrapper predicate return false.
EXTERNAL_CLI="${TMPDIR}/external/wp-codebox"
EXTERNAL_CORE_MODULE="${TMPDIR}/external/contracts.js"
EXTERNAL_CLI_GITHUB_ENV_FILE="${TMPDIR}/external-cli-github-env"
mkdir -p "$(dirname "${EXTERNAL_CLI}")"
cat > "${EXTERNAL_CLI}" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = "--version" ]; then
    printf '%s\n' '0.20.1'
    exit 0
fi
printf '%s\n' 'external wp-codebox'
SH
chmod +x "${EXTERNAL_CLI}"
printf '%s\n' 'module.exports = { runtimeContractManifest() { return { fixture: "external" }; } };' > "${EXTERNAL_CORE_MODULE}"
(
    cd "${EXTENSION_DIR}"
    HOME="${TMPDIR}/external-cli-home" \
    PATH="${FAKE_BIN}:${NODE_BIN_DIR}:/usr/bin:/bin:/usr/sbin:/sbin" \
    GITHUB_ENV="${EXTERNAL_CLI_GITHUB_ENV_FILE}" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/external-cli-install" \
    HOMEBOY_WP_CODEBOX_DOWNLOAD_URL="https://example.test/wp-codebox-cli-linux-x64.tar.gz" \
    FAKE_WP_CODEBOX_RELEASE_MISSING="1" \
    WP_CODEBOX_CLI="${EXTERNAL_CLI}" \
    WP_CODEBOX_CORE_MODULE="${EXTERNAL_CORE_MODULE}" \
    FAKE_EXPECT_WP_CODEBOX_CLI="${EXTERNAL_CLI}" \
    FAKE_EXPECT_WP_CODEBOX_CORE_MODULE="${EXTERNAL_CORE_MODULE}" \
    bash "${ROOT_DIR}/scripts/build/setup.sh" > "${TMPDIR}/external-cli-setup.out"
)

external_cli_source_bin="$(grep '^HOMEBOY_WP_CODEBOX_BIN=' "${EXTERNAL_CLI_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
if [ "${external_cli_source_bin}" != "${TMPDIR}/external-cli-install/source/packages/cli/dist/index.js" ]; then
    echo "Expected incompatible explicit external CLI to use source fallback, got: ${external_cli_source_bin}" >&2
    cat "${TMPDIR}/external-cli-setup.out" >&2
    exit 1
fi

external_cli_core_module="$(grep '^HOMEBOY_WP_CODEBOX_CORE_MODULE=' "${EXTERNAL_CLI_GITHUB_ENV_FILE}" | tail -n 1 | cut -d= -f2-)"
if [ "${external_cli_core_module}" != "${EXTERNAL_CORE_MODULE}" ]; then
    echo "Expected source fallback to preserve the explicit external core module, got: ${external_cli_core_module}" >&2
    cat "${TMPDIR}/external-cli-setup.out" >&2
    exit 1
fi

if [ ! -x "${TMPDIR}/external-cli-install/source/packages/cli/dist/index.js" ]; then
    echo "Expected source fallback to converge the WP Codebox cache" >&2
    cat "${TMPDIR}/external-cli-setup.out" >&2
    exit 1
fi

echo "WP Codebox setup smoke passed"
