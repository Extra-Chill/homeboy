#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

workflow="${root}/.github/workflows/required-gates-ruleset.yml"
grep -Fq 'schedule:' "${workflow}"
grep -Fq 'environment: main-ruleset-administration' "${workflow}"
grep -Fq -- '--operation audit' "${workflow}"
grep -Fq -- '--operation reconcile' "${workflow}"
grep -Fq 'bash .github/validate-required-gates.sh --github' "${workflow}"

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
cat "${GH_STATE}"
EOF
chmod +x "${tmp}/gh"

run() {
  GH_BIN="${tmp}/gh" GH_LOG="${tmp}/gh.log" GH_STATE="${tmp}/state.json" \
    bash "${root}/.github/manage-required-gates-ruleset.sh" "$@"
}

if run --operation audit --evidence "${tmp}/audit.json"; then
  echo 'audit accepted a drifted ruleset' >&2
  exit 1
fi
jq -e '.operation == "audit" and .changed == false and .matched == false' "${tmp}/audit.json" >/dev/null
test ! -s "${tmp}/gh.log" || ! grep -Fq -- '--method PUT' "${tmp}/gh.log"

: > "${tmp}/gh.log"
run --operation reconcile --evidence "${tmp}/reconcile.json"
jq -e '.operation == "reconcile" and .changed == true and .matched == true' "${tmp}/reconcile.json" >/dev/null
grep -Fq -- '--method PUT' "${tmp}/gh.log"
jq -e '[.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]] | length == 8' "${tmp}/state.json" >/dev/null

: > "${tmp}/gh.log"
run --operation audit --evidence "${tmp}/converged.json"
jq -e '.operation == "audit" and .changed == false and .matched == true' "${tmp}/converged.json" >/dev/null
test ! -s "${tmp}/gh.log" || ! grep -Fq -- '--method PUT' "${tmp}/gh.log"
