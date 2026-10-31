#!/usr/bin/env bash
# Behavioral tests for the Node.js component env detector.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DETECT="${SCRIPT_DIR}/../scripts/env/detect.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

detect() {
  (cd "${WORK}/$1" && bash "${DETECT}")
}

case_dir() {
  mkdir -p "${WORK}/$1"
  printf '%s' "$2" > "${WORK}/$1/package.json"
}

expect() {
  local name="$1" expected="$2" actual
  actual="$(detect "${name}")"
  if [ "${actual}" != "${expected}" ]; then
    printf 'FAIL %s: expected %s, got %s\n' "${name}" "${expected}" "${actual}" >&2
    exit 1
  fi
  printf 'PASS: %s -> %s\n' "${name}" "${actual}"
}

case_dir engines-open-floor '{"engines":{"node":">=14.0.0"}}'
expect engines-open-floor '{"runtimes":{"node":{"version":"24"}}}'

case_dir engines-open-above-lts '{"engines":{"node":">=26"}}'
expect engines-open-above-lts '{"runtimes":{"node":{"version":"26"}}}'

case_dir engines-caret '{"engines":{"node":"^22.11.0"}}'
expect engines-caret '{"runtimes":{"node":{"version":"22"}}}'

case_dir engines-alternatives '{"engines":{"node":"^20.18 || ^22"}}'
expect engines-alternatives '{"runtimes":{"node":{"version":"22"}}}'

case_dir engines-bounded '{"engines":{"node":">=18.17 <23"}}'
expect engines-bounded '{"runtimes":{"node":{"version":"22"}}}'

case_dir engines-bounded-minor '{"engines":{"node":">=18 <22.5"}}'
expect engines-bounded-minor '{"runtimes":{"node":{"version":"22"}}}'

case_dir engines-hyphen '{"engines":{"node":"16 - 20"}}'
expect engines-hyphen '{"runtimes":{"node":{"version":"20"}}}'

case_dir engines-x-range '{"engines":{"node":"20.x"}}'
expect engines-x-range '{"runtimes":{"node":{"version":"20"}}}'

case_dir engines-legacy-alternatives '{"engines":{"node":"^8.16.0 || ^10.6.0 || >=11.0.0"}}'
expect engines-legacy-alternatives '{"runtimes":{"node":{"version":"24"}}}'

case_dir engines-upper-only '{"engines":{"node":"<21"}}'
expect engines-upper-only '{"runtimes":{"node":{"version":"20"}}}'

case_dir nvmrc-wins '{"engines":{"node":">=20"}}'
printf 'v22.23.2\n' > "${WORK}/nvmrc-wins/.nvmrc"
expect nvmrc-wins '{"runtimes":{"node":{"version":"22.23.2"}}}'

case_dir node-version-file '{"engines":{"node":">=20"}}'
printf '24\n' > "${WORK}/node-version-file/.node-version"
expect node-version-file '{"runtimes":{"node":{"version":"24"}}}'

case_dir alias-ignored '{"engines":{"node":"^20"}}'
printf 'lts/*\n' > "${WORK}/alias-ignored/.nvmrc"
expect alias-ignored '{"runtimes":{"node":{"version":"20"}}}'

case_dir undeclared '{"name":"fixture"}'
expect undeclared '{}'

mkdir -p "${WORK}/no-manifest"
expect no-manifest '{}'

case_dir malformed '{not json'
expect malformed '{}'

printf 'PASS: component-env-detect-smoke\n'
