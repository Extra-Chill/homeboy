#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BUILD_SCRIPT="${EXTENSION_ROOT}/scripts/release/build.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

BIN_DIR="${TMP_DIR}/bin"
mkdir -p "${BIN_DIR}"
cat >"${BIN_DIR}/npm" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
  run)
    script="${2:-}"
    printf '%s\n' "${script}" >>"${NPM_CALLS_FILE}"
    if [[ "${script}" == "release:package" ]]; then
      case "${PACKAGE_FIXTURE_MODE:-valid}" in
        valid)
          touch plugin.zip cli.tar.gz
          printf '%s\n' '> fixture@1.0.0 release:package' '> node package.js' '' '[{"path":"plugin.zip","type":"wordpress-plugin-zip"},{"path":"cli.tar.gz","type":"node-cli-tarball","platform":"linux-x64"}]'
          ;;
        duplicate)
          touch fixture-1.0.0.tgz
          printf '%s\n' '[{"path":"fixture-1.0.0.tgz","type":"npm_tarball","platform":null}]'
          ;;
        malformed)
          printf '%s\n' 'not-json'
          ;;
        scalar)
          printf '%s\n' '"artifact.zip"'
          ;;
        empty)
          :
          ;;
        missing)
          printf '%s\n' '[{"path":"missing.zip","type":"wordpress-plugin-zip"}]'
          ;;
        failure)
          printf '%s\n' 'package failed'
          exit 9
          ;;
      esac
    fi
    ;;
  pack)
    printf '%s\n' 'pack' >>"${NPM_CALLS_FILE}"
    touch fixture-1.0.0.tgz
    printf '%s\n' '> fixture@1.0.0 prepare' '[{"filename":"fixture-1.0.0.tgz"}]'
    ;;
  *)
    echo "Unexpected npm invocation: $*" >&2
    exit 64
    ;;
esac
SH
chmod +x "${BIN_DIR}/npm"

write_package() {
  local root="$1"
  local scripts="$2"
  mkdir -p "${root}"
  jq -cn --argjson scripts "${scripts}" '{name:"fixture",version:"1.0.0",scripts:$scripts}' >"${root}/package.json"
  : >"${root}/npm-calls.log"
}

run_build() {
  local root="$1"
  shift
  (
    cd "${root}"
    env PATH="${BIN_DIR}:${PATH}" NPM_CALLS_FILE="${root}/npm-calls.log" "$@" bash "${BUILD_SCRIPT}"
  )
}

configured="${TMP_DIR}/configured"
write_package "${configured}" '{"build":"fixture-build","release:package":"fixture-package"}'
configured_output=$(run_build "${configured}" HOMEBOY_SETTINGS_JSON='{"config":{"release_package_script":"release:package"}}')
configured_expected='[{"path":"plugin.zip","type":"wordpress-plugin-zip"},{"path":"cli.tar.gz","type":"node-cli-tarball","platform":"linux-x64"},{"path":"fixture-1.0.0.tgz","type":"npm_tarball","platform":null}]'
[[ "$(printf '%s' "${configured_output}" | jq -c .)" == "${configured_expected}" ]]
[[ "$(cat "${configured}/npm-calls.log")" == $'release:package\npack' ]]

default_root="${TMP_DIR}/default"
write_package "${default_root}" '{"build":"fixture-build"}'
default_output=$(run_build "${default_root}")
[[ "$(printf '%s' "${default_output}" | jq -c .)" == '[{"path":"fixture-1.0.0.tgz","type":"npm_tarball","platform":null}]' ]]
[[ "$(cat "${default_root}/npm-calls.log")" == $'build\npack' ]]

duplicate_root="${TMP_DIR}/duplicate"
write_package "${duplicate_root}" '{"release:package":"fixture-package"}'
duplicate_output=$(run_build "${duplicate_root}" HOMEBOY_SETTINGS_JSON='{"config":{"release_package_script":"release:package"}}' PACKAGE_FIXTURE_MODE=duplicate)
[[ "$(printf '%s' "${duplicate_output}" | jq 'length')" == "1" ]]

for mode in malformed scalar empty missing failure; do
  failure_root="${TMP_DIR}/${mode}"
  write_package "${failure_root}" '{"release:package":"fixture-package"}'
  if run_build "${failure_root}" HOMEBOY_SETTINGS_JSON='{"config":{"release_package_script":"release:package"}}' PACKAGE_FIXTURE_MODE="${mode}" >/dev/null 2>&1; then
    echo "Configured package mode unexpectedly succeeded: ${mode}" >&2
    exit 1
  fi
done

absent_root="${TMP_DIR}/absent"
write_package "${absent_root}" '{}'
if run_build "${absent_root}" HOMEBOY_SETTINGS_JSON='{"config":{"release_package_script":"release:package"}}' >/dev/null 2>&1; then
  echo "Missing configured package script unexpectedly succeeded" >&2
  exit 1
fi

echo "Node.js release package manifest smoke passed"
