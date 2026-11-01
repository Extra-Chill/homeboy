#!/usr/bin/env bash
# Behavioral tests for enable-package-manager.sh. Uses the real corepack that
# ships with Node, so the declared version must actually become runnable.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="${ROOT_DIR}/scripts/setup/enable-package-manager.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

# Isolate corepack's shims and cache so the test cannot see, or leave behind,
# a package manager from the host.
export COREPACK_HOME="${WORK}/corepack"
BIN="${WORK}/bin"
mkdir -p "${BIN}"
export PATH="${BIN}:${PATH}"
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0

enable() {
  (cd "${WORK}" && COMPONENT_DIR="$1" COREPACK_INSTALL_DIR="${BIN}" bash "${SCRIPT}")
}

# A pinned pnpm becomes runnable at exactly the declared version.
mkdir -p "${WORK}/pnpm-component"
printf '%s\n' '{"name":"fixture","packageManager":"pnpm@9.15.9"}' > "${WORK}/pnpm-component/package.json"
out="$(enable pnpm-component)"
grep -Fq 'Enabled pnpm 9.15.9' <<<"${out}"
[ "$(cd "${WORK}/pnpm-component" && pnpm --version)" = "9.15.9" ]
printf 'PASS: declared pnpm version is enabled and runnable\n'

# No package.json: nothing to do.
mkdir -p "${WORK}/no-manifest"
[ -z "$(enable no-manifest)" ]
printf 'PASS: component without package.json is a no-op\n'

# package.json without packageManager: npm is already present.
mkdir -p "${WORK}/npm-default"
printf '%s\n' '{"name":"fixture"}' > "${WORK}/npm-default/package.json"
[ -z "$(enable npm-default)" ]
printf 'PASS: undeclared package manager is a no-op\n'

# An unpinned declaration is refused rather than resolved to an arbitrary version.
mkdir -p "${WORK}/unpinned"
printf '%s\n' '{"name":"fixture","packageManager":"pnpm"}' > "${WORK}/unpinned/package.json"
if enable unpinned >/dev/null 2>&1; then
  printf 'FAIL: an unpinned packageManager must be refused\n' >&2
  exit 1
fi
printf 'PASS: unpinned packageManager is refused\n'

# An unsupported package manager is refused.
mkdir -p "${WORK}/unsupported"
printf '%s\n' '{"name":"fixture","packageManager":"bun@1.1.0"}' > "${WORK}/unsupported/package.json"
if enable unsupported >/dev/null 2>&1; then
  printf 'FAIL: an unsupported packageManager must be refused\n' >&2
  exit 1
fi
printf 'PASS: unsupported packageManager is refused\n'
