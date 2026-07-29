#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

workflow="${root}/.github/workflows/required-gates-ruleset.yml"
grep -Fq 'workflow_dispatch:' "${workflow}"
grep -Fq 'contents: read' "${workflow}"
grep -Fq 'environment: main-ruleset-administration' "${workflow}"
grep -Fq "test \"\${GITHUB_REPOSITORY}\" = 'Extra-Chill/homeboy'" "${workflow}"
test "$(grep -Fc 'ref: main' "${workflow}")" -eq 2
grep -Fq 'confirmation=APPLY_REQUIRED_GATES' "${root}/docs/operations/required-ci-gates.md"
grep -Fq 'confirmation=EMERGENCY_BYPASS_REQUIRED_GATES' "${root}/docs/operations/required-ci-gates.md"

jq 'del(.rules[] | select(.type == "required_status_checks"))' \
  "${root}/.github/required-gates-ruleset.json" > "${tmp}/state.json"

cat > "${tmp}/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${GH_LOG}"
if [[ " $* " == *" --method PUT "* ]]; then
  while [ "$#" -gt 0 ]; do
    if [ "$1" = '--input' ]; then cp "$2" "${GH_STATE}"; exit 0; fi
    shift
  done
fi
if [[ "$*" == *'/issues/'* ]]; then
  printf '%s\n' '{"state":"open","title":"Emergency CI bypass: test"}'
else
  cat "${GH_STATE}"
fi
EOF
chmod +x "${tmp}/gh"

run() {
  GH_BIN="${tmp}/gh" GH_LOG="${tmp}/gh.log" GH_STATE="${tmp}/state.json" \
    GITHUB_REPOSITORY=Extra-Chill/homeboy \
    bash "${root}/.github/manage-required-gates-ruleset.sh" "$@"
}

run --operation dry-run --dry-run --evidence "${tmp}/dry-run.json"
test ! -s "${tmp}/gh.log" || ! grep -Fq -- '--method PUT' "${tmp}/gh.log"
jq -e '.operation == "dry-run" and .mode == "dry-run" and .before == .after' "${tmp}/dry-run.json" >/dev/null

: > "${tmp}/gh.log"
if run --operation apply --apply --confirmation WRONG --evidence "${tmp}/rejected.json"; then
  echo 'wrong confirmation unexpectedly applied the ruleset' >&2
  exit 1
fi
test ! -s "${tmp}/gh.log"

run --operation apply --apply --confirmation APPLY_REQUIRED_GATES --evidence "${tmp}/apply.json"
grep -Fq -- '--method PUT' "${tmp}/gh.log"
jq -e '[.after.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]] | length == 8' "${tmp}/apply.json" >/dev/null

: > "${tmp}/gh.log"
run --operation emergency-bypass --apply --confirmation EMERGENCY_BYPASS_REQUIRED_GATES --emergency-issue 10795 --evidence "${tmp}/emergency.json"
grep -Fq -- '/issues/10795' "${tmp}/gh.log"
jq -e '[.after.rules[] | select(.type == "required_status_checks")] | length == 0' "${tmp}/emergency.json" >/dev/null
