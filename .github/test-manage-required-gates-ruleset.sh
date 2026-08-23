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
grep -Fq 'bash .github/reconcile-required-gates-ruleset.sh --evidence required-gates-ruleset-reconcile.json' "${workflow}"
grep -Fq -- '--operation reconcile' "${root}/.github/reconcile-required-gates-ruleset.sh"
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
  if [ "${GH_FAIL_PUT:-}" = true ]; then exit 1; fi
  while [ "$#" -gt 0 ]; do
    if [ "$1" = '--input' ]; then cp "$2" "${GH_STATE}"; exit 0; fi
    shift
  done
fi
if [[ " $* " == *'/environments/main-ruleset-administration'* ]]; then
  cat "${GH_ENVIRONMENT}"
  exit 0
fi
if [[ " $* " == *'/commits/main'* ]]; then
  printf '{"sha":"%s"}\n' "${GH_MAIN_SHA}"
  exit 0
fi
if [[ " $* " == *'/check-runs?'* ]]; then
  cat "${GH_CHECKS}"
  exit 0
fi
cat "${GH_STATE}"
EOF
chmod +x "${tmp}/gh"

run() {
  GH_BIN="${tmp}/gh" GH_LOG="${tmp}/gh.log" GH_STATE="${tmp}/state.json" \
    bash "${root}/.github/manage-required-gates-ruleset.sh" "$@"
}

printf '%s\n' '{"can_admins_bypass":false,"protection_rules":[{"type":"required_reviewers"}]}' > "${tmp}/environment.json"
printf '%s\n' '{"check_runs":[{"name":"Test","app":{"id":15368},"conclusion":"success"}]}' > "${tmp}/checks.json"
jq '.rules' "${root}/.github/required-gates-ruleset.json" > "${tmp}/effective-rules.json"

run_reconcile() {
  GH_BIN="${tmp}/gh" GH_LOG="${tmp}/gh.log" GH_STATE="${tmp}/state.json" \
    GH_ENVIRONMENT="${tmp}/environment.json" GH_MAIN_SHA="$(git -C "${root}" rev-parse HEAD)" \
    GH_CHECKS="${1}" GH_FAIL_PUT="${GH_FAIL_PUT:-}" GH_TOKEN=fixture-token GITHUB_REPOSITORY=Extra-Chill/homeboy \
    REQUIRED_GATES_LIVE_RULES="${tmp}/effective-rules.json" REQUIRED_GATES_LIVE_RULESET="${tmp}/state.json" \
    bash "${root}/.github/reconcile-required-gates-ruleset.sh" --evidence "${2}"
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

# The PR UI prefixes a required context with `homeboy / `, but the main-push
# check-runs API exposes the canonical GitHub Actions job name `Test`.
jq 'del(.rules[] | select(.type == "required_status_checks"))' "${root}/.github/required-gates-ruleset.json" > "${tmp}/state.json"
: > "${tmp}/gh.log"
run_reconcile "${tmp}/checks.json" "${tmp}/successful-reconcile.json"
jq -e '.operation == "reconcile" and .matched == true' "${tmp}/successful-reconcile.json" >/dev/null
grep -Fq -- '--method PUT' "${tmp}/gh.log"

printf '%s\n' '{"check_runs":[{"name":"homeboy / Test","app":{"id":15368},"conclusion":"success"}]}' > "${tmp}/pr-display-checks.json"
if run_reconcile "${tmp}/pr-display-checks.json" "${tmp}/pr-display-failure.json"; then
  echo 'reconcile accepted a PR display context as a main check-run name' >&2
  exit 1
fi
jq -e '.outcome == "failed" and .stage == "check-run" and .preflight.check_name == "Test" and .preflight.check_app_id == "15368"' "${tmp}/pr-display-failure.json" >/dev/null

printf '%s\n' '{"check_runs":[{"name":"Test","app":{"id":99999},"conclusion":"success"}]}' > "${tmp}/spoofed-checks.json"
if run_reconcile "${tmp}/spoofed-checks.json" "${tmp}/spoofed-failure.json"; then
  echo 'reconcile accepted a spoofed successful Test check' >&2
  exit 1
fi
jq -e '.outcome == "failed" and .stage == "check-run"' "${tmp}/spoofed-failure.json" >/dev/null

printf '%s\n' '{"check_runs":[]}' > "${tmp}/missing-checks.json"
if run_reconcile "${tmp}/missing-checks.json" "${tmp}/missing-check-failure.json"; then
  echo 'reconcile accepted a missing required Test check' >&2
  exit 1
fi
jq -e '.outcome == "failed" and .stage == "check-run" and .reason == "canonical successful required check is absent"' "${tmp}/missing-check-failure.json" >/dev/null

if GH_BIN="${tmp}/gh" GH_LOG="${tmp}/gh.log" GH_STATE="${tmp}/state.json" GITHUB_REPOSITORY=Extra-Chill/homeboy \
  bash "${root}/.github/reconcile-required-gates-ruleset.sh" --evidence "${tmp}/token-failure.json"; then
  echo 'reconcile accepted a missing administration token' >&2
  exit 1
fi
jq -e '.outcome == "failed" and .stage == "token"' "${tmp}/token-failure.json" >/dev/null

printf '%s\n' '{"can_admins_bypass":true,"protection_rules":[]}' > "${tmp}/environment.json"
if run_reconcile "${tmp}/checks.json" "${tmp}/environment-failure.json"; then
  echo 'reconcile accepted an unprotected environment' >&2
  exit 1
fi
jq -e '.outcome == "failed" and .stage == "environment"' "${tmp}/environment-failure.json" >/dev/null

printf '%s\n' '{"can_admins_bypass":false,"protection_rules":[{"type":"required_reviewers"}]}' > "${tmp}/environment.json"
jq 'del(.rules[] | select(.type == "required_status_checks"))' "${root}/.github/required-gates-ruleset.json" > "${tmp}/state.json"
if GH_FAIL_PUT=true run_reconcile "${tmp}/checks.json" "${tmp}/write-failure.json"; then
  echo 'reconcile accepted a failed ruleset update' >&2
  exit 1
fi
jq -e '.outcome == "failed" and .stage == "reconcile" and .reason == "command failed before detailed evidence was written"' "${tmp}/write-failure.json" >/dev/null
