#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

workflow="${root}/.github/workflows/required-gates-ruleset.yml"
grep -Fq 'workflow_dispatch:' "${workflow}"
grep -Fq 'schedule:' "${workflow}"
grep -Fq "cron: '17 * * * *'" "${workflow}"
grep -Fq 'environment: main-ruleset-administration' "${workflow}"
grep -Fq -- '--operation audit' "${workflow}"
grep -Fq -- '--operation reconcile' "${workflow}"
grep -Fq "github.ref == 'refs/heads/main'" "${workflow}"
grep -Fq 'environments/main-ruleset-administration' "${workflow}"
grep -Fq 'required_reviewers' "${workflow}"
grep -Fq 'can_admins_bypass == false' "${workflow}"
grep -Fq 'commits/${head}/check-runs' "${workflow}"
grep -Fq '.name == "homeboy / Test" and .app.id == 15368 and .conclusion == "success"' "${workflow}"
grep -Fq 'commits/main' "${workflow}"
grep -Fq 'test "${head}" = "${current_main}"' "${workflow}"
grep -Fq 'bash .github/validate-required-gates.sh --github' "${workflow}"
! grep -Fq 'gh api --method PUT' "${root}/docs/operations/required-ci-gates.md"
! grep -Fq 'gh api --method PUT' "${root}/.github/validate-required-gates.sh"

preflight_query='any(.check_runs[]; .name == "homeboy / Test" and .app.id == 15368 and .conclusion == "success")'
if jq -e "${preflight_query}" <<<'{"check_runs":[{"name":"homeboy / Test","app":{"id":99999},"conclusion":"success"}]}' >/dev/null; then
  echo 'ruleset preflight accepted a spoofed successful Test check' >&2
  exit 1
fi
jq -e "${preflight_query}" <<<'{"check_runs":[{"name":"homeboy / Test","app":{"id":15368},"conclusion":"success"}]}' >/dev/null

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
jq -e '.operation == "audit" and .policy_version == "required-gates-ruleset/v1" and (.policy_revision | length == 40) and (.policy_sha256 | length == 64) and .changed == false and .matched == false' "${tmp}/audit.json" >/dev/null
test ! -s "${tmp}/gh.log" || ! grep -Fq -- '--method PUT' "${tmp}/gh.log"

: > "${tmp}/gh.log"
run --operation reconcile --evidence "${tmp}/reconcile.json"
jq -e '.operation == "reconcile" and .changed == true and .matched == true' "${tmp}/reconcile.json" >/dev/null
grep -Fq -- '--method PUT' "${tmp}/gh.log"
jq -e '[.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]] | length == 8' "${tmp}/state.json" >/dev/null
jq -e '.bypass_actors == []' "${tmp}/state.json" >/dev/null

: > "${tmp}/gh.log"
run --operation audit --evidence "${tmp}/converged.json"
jq -e '.operation == "audit" and .changed == false and .matched == true' "${tmp}/converged.json" >/dev/null
test ! -s "${tmp}/gh.log" || ! grep -Fq -- '--method PUT' "${tmp}/gh.log"
