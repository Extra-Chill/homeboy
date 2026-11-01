#!/usr/bin/env bash

# Tests provisioning reuse across repeated action invocations in one job.
#
# The `homeboy` stub only prints a version read from a file. It never invokes
# any action script, so nothing here can recurse.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="${ROOT_DIR}/scripts/setup/provisioning-reuse.sh"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

mkdir -p "${TMP_DIR}/bin" "${TMP_DIR}/home/.config/homeboy/extensions/wordpress" "${TMP_DIR}/runner-temp" "${TMP_DIR}/component"
printf 'homeboy 1.2.3\n' > "${TMP_DIR}/homeboy-version"
cat > "${TMP_DIR}/bin/homeboy" <<'STUB'
#!/bin/sh
[ "$1" = "--version" ] && cat "${HOMEBOY_STUB_VERSION_FILE}"
exit 0
STUB
chmod +x "${TMP_DIR}/bin/homeboy"
printf 'https://github.com/Extra-Chill/homeboy\n' > "${TMP_DIR}/home/.config/homeboy/extensions/wordpress/.source-url"
printf 'abc1234\n' > "${TMP_DIR}/home/.config/homeboy/extensions/wordpress/.source-revision"
printf '{"id":"fixture","extensions":{"wordpress":{}}}\n' > "${TMP_DIR}/component/homeboy.json"

export PATH="${TMP_DIR}/bin:${PATH}"
export HOME="${TMP_DIR}/home"
export HOMEBOY_STUB_VERSION_FILE="${TMP_DIR}/homeboy-version"
export RUNNER_TEMP="${TMP_DIR}/runner-temp"
export GITHUB_ACTION_PATH="${ROOT_DIR}"
export COMPONENT_DIR="${TMP_DIR}/component"

fail() {
  printf 'FAIL: %s\n' "$1"
  exit 1
}

inputs() {
  printf 'version=%s\nextension=wordpress\nextension-source=%s\n' "$1" "$2"
}

# Runs `check` and prints its GITHUB_OUTPUT.
check() {
  local out="${TMP_DIR}/check-output"
  : > "${out}"
  PROVISIONING_INPUTS="$(inputs "$1" "$2")" GITHUB_OUTPUT="${out}" bash "${SCRIPT}" check >/dev/null
  cat "${out}"
}

output_value() {
  printf '%s\n' "$2" | sed -n "s/^$1=//p"
}

record() {
  : > "${TMP_DIR}/record-output"
  GITHUB_OUTPUT="${TMP_DIR}/record-output" \
    PROVISIONING_KEY="$1" \
    BINARY_SOURCE=release \
    HOMEBOY_VERSION_OUTPUT='homeboy 1.2.3' \
    TOOLING_IDENTITY='homeboy-1.2.3-abc1234' \
    PORTABLE_PHP_OUTPUT='' \
    PORTABLE_NODE_OUTPUT='' \
    INSTALLED_EXTENSION=wordpress \
    bash "${SCRIPT}" record >/dev/null
}

source_a="https://github.com/Extra-Chill/homeboy"
source_b="https://github.com/example/extensions"

first="$(check latest "${source_a}")"
[ "$(output_value reuse "${first}")" = false ] || fail 'first invocation provisions'
key="$(output_value key "${first}")"
[ -n "${key}" ] || fail 'check publishes a key'
echo 'PASS: first invocation provisions'

record "${key}"
[ -f "${RUNNER_TEMP}/homeboy-action-provisioning/${key}" ] || fail 'record writes the marker'

second="$(check latest "${source_a}")"
[ "$(output_value reuse "${second}")" = true ] || fail 'second invocation with same inputs reuses provisioning'
[ "$(output_value binary-source "${second}")" = release ] || fail 'reuse republishes binary-source'
[ "$(output_value homeboy-version "${second}")" = 'homeboy 1.2.3' ] || fail 'reuse republishes homeboy-version'
[ "$(output_value tooling-identity "${second}")" = 'homeboy-1.2.3-abc1234' ] || fail 'reuse republishes tooling-identity'
grep -q '^portable-php=$' <<< "${second}" || fail 'reuse republishes portable-php'
echo 'PASS: same inputs reuse provisioning and republish outputs'

[ "$(output_value reuse "$(check 9.9.9 "${source_a}")")" = false ] || fail 'changed version re-provisions'
echo 'PASS: changed version re-provisions'

[ "$(output_value reuse "$(check latest "${source_b}")")" = false ] || fail 'changed extension-source re-provisions'
echo 'PASS: changed extension-source re-provisions'

printf '{"id":"fixture","extensions":{"nodejs":{}}}\n' > "${COMPONENT_DIR}/homeboy.json"
[ "$(output_value reuse "$(check latest "${source_a}")")" = false ] || fail 'changed homeboy.json re-provisions'
printf '{"id":"fixture","extensions":{"wordpress":{}}}\n' > "${COMPONENT_DIR}/homeboy.json"
[ "$(output_value reuse "$(check latest "${source_a}")")" = true ] || fail 'restored homeboy.json reuses again'
echo 'PASS: homeboy.json is part of the key'

# A later invocation with other inputs reinstalled the extension in place.
printf 'def5678\n' > "${HOME}/.config/homeboy/extensions/wordpress/.source-revision"
[ "$(output_value reuse "$(check latest "${source_a}")")" = false ] || fail 'reinstalled extension invalidates reuse'
[ ! -f "${RUNNER_TEMP}/homeboy-action-provisioning/${key}" ] || fail 'stale marker is removed'
printf 'abc1234\n' > "${HOME}/.config/homeboy/extensions/wordpress/.source-revision"
echo 'PASS: reinstalled extension invalidates reuse'

record "${key}"
printf 'homeboy 2.0.0\n' > "${HOMEBOY_STUB_VERSION_FILE}"
[ "$(output_value reuse "$(check latest "${source_a}")")" = false ] || fail 'replaced homeboy binary invalidates reuse'
printf 'homeboy 1.2.3\n' > "${HOMEBOY_STUB_VERSION_FILE}"
echo 'PASS: replaced homeboy binary invalidates reuse'

record "${key}"
rm -rf "${HOME}/.config/homeboy/extensions/wordpress"
[ "$(output_value reuse "$(check latest "${source_a}")")" = false ] || fail 'removed extension invalidates reuse'
echo 'PASS: removed extension invalidates reuse'

no_temp_out="${TMP_DIR}/no-temp-output"
: > "${no_temp_out}"
env -u RUNNER_TEMP PROVISIONING_INPUTS="$(inputs latest "${source_a}")" GITHUB_OUTPUT="${no_temp_out}" bash "${SCRIPT}" check >/dev/null
grep -qx 'reuse=false' "${no_temp_out}" || fail 'missing RUNNER_TEMP disables reuse'
echo 'PASS: missing RUNNER_TEMP disables reuse'

# Every provisioning step between the check and the record must skip on reuse,
# and every consumer of a skipped step's output must fall back to the
# republished value.
python3 - "${ROOT_DIR}/action.yml" <<'PY'
import sys
import yaml

action = yaml.safe_load(open(sys.argv[1], encoding="utf-8"))
steps = action["runs"]["steps"]
names = [step.get("name") for step in steps]
start = names.index("Check provisioning reuse")
end = names.index("Record provisioning for reuse")
gate = "steps.provisioning.outputs.reuse != 'true'"
# nextest installs only for Rust test runs and is cheap to repeat.
ungated = {"Install cargo-nextest"}

errors = []
for step in steps[start + 1 : end]:
    if step["name"] in ungated:
        continue
    if gate not in str(step.get("if", "")):
        errors.append(f"step '{step['name']}' does not skip on provisioning reuse")

if gate not in str(steps[end].get("if", "")):
    errors.append("record step must not re-record a reused invocation")

skipped_ids = {"resolve-binary", "tooling-metadata", "detect-env"}
text = open(sys.argv[1], encoding="utf-8").read()
# Steps after setup run on reuse too, so they must fall back to the
# republished value when the producing step was skipped.
for line in text[text.index("- name: Finish dependency/build setup timing"):].splitlines():
    for step_id in skipped_ids:
        if f"steps.{step_id}.outputs." in line and "steps.provisioning.outputs." not in line:
            errors.append(f"later consumer lacks reuse fallback: {line.strip()}")
outputs = action["outputs"]
for name in ("binary-source", "homeboy-version", "tooling-identity"):
    if "steps.provisioning.outputs." not in outputs[name]["value"]:
        errors.append(f"action output {name} lacks reuse fallback")

if errors:
    raise SystemExit("FAIL: " + "\nFAIL: ".join(errors))
print("PASS: action.yml gates provisioning steps and republishes outputs on reuse")
PY

echo 'Provisioning reuse tests passed'
