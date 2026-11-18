#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT
mkdir -p "${tmp}/bin" "${tmp}/component"
printf 'fixture lock\n' > "${tmp}/component/Cargo.lock"

cat > "${tmp}/bin/homeboy" <<'SH'
#!/usr/bin/env bash
printf '%s\n' '{"data":{"extension":{"ci_cache":{"namespace":"cargo","key_files":["Cargo.lock"],"paths":[{"root":"home","path":".cargo/registry"},{"root":"homeboy-data","path":"cargo-targets"},{"root":"component","path":"target","env":"CARGO_TARGET_DIR"}]}}}}'
SH
chmod +x "${tmp}/bin/homeboy"

output="${tmp}/output"
PATH="${tmp}/bin:${PATH}" \
  HOME="${tmp}/home" \
  XDG_DATA_HOME="${tmp}/data" \
  COMPONENT_DIR="${tmp}/component" \
  PORTABLE_EXTENSION=rust \
  GITHUB_ENV="${tmp}/env" \
  GITHUB_OUTPUT="${output}" \
  bash "${SCRIPT_DIR}/resolve-extension-cache.sh"

grep -qx 'enabled=true' "${output}"
grep -qx 'namespace=cargo' "${output}"
grep -qx "${tmp}/home/.cargo/registry" "${output}"
grep -qx "${tmp}/data/homeboy/cargo-targets" "${output}"
grep -qx "${tmp}/component/target" "${output}"
grep -q '^fingerprint=[0-9a-f]\{64\}$' "${output}"
grep -qx "CARGO_TARGET_DIR=${tmp}/component/target" "${tmp}/env"

cat > "${tmp}/bin/homeboy" <<'SH'
#!/usr/bin/env bash
printf '%s\n' '{"data":{"extension":{"ci_cache":{"namespace":"cargo","key_files":["Cargo.lock"],"paths":[{"root":"home","path":"../secrets"}]}}}}'
SH
if PATH="${tmp}/bin:${PATH}" HOME="${tmp}/home" COMPONENT_DIR="${tmp}/component" PORTABLE_EXTENSION=rust GITHUB_ENV="${tmp}/unsafe-env" GITHUB_OUTPUT="${tmp}/unsafe-output" bash "${SCRIPT_DIR}/resolve-extension-cache.sh"; then
  echo 'FAIL: unsafe cache path was accepted'
  exit 1
fi

grep -qx 'rolling=false' "${output}"

# A lockfile-keyed cache with no key file present stays disabled.
rm -f "${tmp}/component/Cargo.lock"
cat > "${tmp}/bin/homeboy" <<'SH'
#!/usr/bin/env bash
printf '%s\n' '{"data":{"extension":{"ci_cache":{"namespace":"cargo","key_files":["Cargo.lock"],"paths":[{"root":"home","path":".cargo/registry"}]}}}}'
SH
PATH="${tmp}/bin:${PATH}" HOME="${tmp}/home" COMPONENT_DIR="${tmp}/component" PORTABLE_EXTENSION=rust GITHUB_ENV="${tmp}/nokey-env" GITHUB_OUTPUT="${tmp}/nokey-output" bash "${SCRIPT_DIR}/resolve-extension-cache.sh"
grep -qx 'enabled=false' "${tmp}/nokey-output"

# A rolling download cache needs no key files: the deploy that fills it may
# hydrate components whose lockfiles live outside the configured component.
cat > "${tmp}/bin/homeboy" <<'SH'
#!/usr/bin/env bash
printf '%s\n' '{"data":{"extension":{"ci_cache":{"namespace":"downloads","rolling":true,"paths":[{"root":"home","path":".store/_content"},{"root":"home","path":".cache/pm","env":"PM_CACHE_DIR"}]}}}}'
SH
run_rolling() {
  PATH="${tmp}/bin:${PATH}" HOME="${tmp}/home" COMPONENT_DIR="${tmp}/component" PORTABLE_EXTENSION=downloads GITHUB_ENV="${tmp}/rolling-env" GITHUB_OUTPUT="$1" bash "${SCRIPT_DIR}/resolve-extension-cache.sh"
}
run_rolling "${tmp}/rolling-output"
grep -qx 'enabled=true' "${tmp}/rolling-output"
grep -qx 'rolling=true' "${tmp}/rolling-output"
grep -qx 'populated=false' "${tmp}/rolling-output"
grep -qx 'namespace=downloads' "${tmp}/rolling-output"
grep -qx "${tmp}/home/.store/_content" "${tmp}/rolling-output"
grep -qx "${tmp}/home/.cache/pm" "${tmp}/rolling-output"
grep -qx "PM_CACHE_DIR=${tmp}/home/.cache/pm" "${tmp}/rolling-env"

# Only one store holding files is not enough to skip the restore.
mkdir -p "${tmp}/home/.store/_content" "${tmp}/home/.cache/pm"
printf 'blob\n' > "${tmp}/home/.store/_content/a"
run_rolling "${tmp}/partial-output"
grep -qx 'populated=false' "${tmp}/partial-output"

# Every store already holding files (e.g. an earlier invocation in this job)
# lets the action skip the restore.
printf 'dist\n' > "${tmp}/home/.cache/pm/b"
run_rolling "${tmp}/populated-output"
grep -qx 'populated=true' "${tmp}/populated-output"

echo 'Extension cache resolver tests passed'

