#!/usr/bin/env bash
#
# Main-branch gate contract validator.
#
# ── Two claims, not one (#11084) ──
#
# This script can answer two questions, and they are NOT the same question:
#
#   declaration - every context named by the versioned payload is emitted by
#                 `.github/workflows/ci.yml` on every pull request, and every
#                 skippable gate job is wired into the terminal execution gate.
#                 This is repository content, so a PR can both break it and fix
#                 it, and it is checked from the checkout with no network.
#   enforcement - GitHub actually *requires* those contexts before `main` can be
#                 updated. This is repository STATE. A pull request cannot
#                 change it and, until #10795's payload is installed by an
#                 administrator, it can be false while the declaration is
#                 perfect.
#
# There is a THIRD question neither half can answer, because both are evaluated
# before the gates finish: did the declared gates actually EXECUTE? PR #12567
# merged with this check green in a run whose gates were all cancelled, then
# skipped entirely in the replacement run that concluded `success` (#12573).
# `.github/ci-required-gates-executed.sh` owns that claim at the END of a run.
# What a pull request CAN break about it — the wiring that makes the terminal
# gate cover every skippable gate job — is checked below, fail-closed, because
# it is repository content like the rest of the declaration.
#
# Before #11084 the CI job ran `--local` and reported a plain green tick named
# "Required Gates Policy". That tick asserted the second claim while only ever
# checking the first: on 2026-08-01 PR #11069 merged nine minutes before its
# final `homeboy / Test` verdict (which was red) while this check was green,
# because `repos/Extra-Chill/homeboy/rules/branches/main` carried no
# required-status-check rule at all.
#
# The repair is reporting, not enforcement. This repository merges fast on
# purpose and a post-merge guard was removed on purpose; making merges block
# would re-litigate a settled design decision. What is not acceptable is a check
# that *claims* a guarantee nobody installed. So `--report` states the live
# enforcement outcome loudly and exits 0 regardless: nothing here can newly
# block a pull request.
#
# ── Modes ──
#
#   --local   Declaration only. Fails closed. No network.
#   --report  Declaration fails closed; enforcement is PROBED and REPORTED and
#             never enforced. Always exits 0 once the declaration passes. This
#             is what CI runs, so its green tick claims only what it checked.
#   --github  Declaration and enforcement both fail closed. The administrator
#             verification path from docs/operations/required-ci-gates.md, run
#             by a human after applying the payload — deliberately not CI.
#
# ── Enforcement outcomes ──
#
# Vocabulary deliberately mirrors `.github/release-quality-policy.sh`, which
# already had to learn that "the check passed" and "the check measured
# something" are different facts:
#
#   enforced    live required contexts == declared set, strict policy on.
#   bypassable  as `enforced`, but actors can bypass the ruleset.
#   divergent   a required-status-checks rule exists and disagrees.
#   unenforced  live state is readable and requires NO checks. A measured zero.
#   unverified  live state could not be read. NEVER reported as enforced.
#
# ── Fixture hooks ──
#
# REQUIRED_GATES_CONFIG / REQUIRED_GATES_WORKFLOW override the declaration
# inputs; REQUIRED_GATES_LIVE_RULES / REQUIRED_GATES_LIVE_RULESET substitute a
# JSON file for the two live API reads; REQUIRED_GATES_HEAD_SHA pins the
# recorded head. `tests/required_gates_policy_test.rs` uses them to pin every
# outcome above without a network or a token.

set -euo pipefail

mode="${1:---local}"

case "${mode}" in
  --local | --report | --github) ;;
  *)
    echo "usage: bash .github/validate-required-gates.sh [--local|--report|--github]" >&2
    exit 2
    ;;
esac

config="${REQUIRED_GATES_CONFIG:-.github/required-gates-ruleset.json}"
ci_workflow="${REQUIRED_GATES_WORKFLOW:-.github/workflows/ci.yml}"

if [ ! -f "${config}" ] || [ ! -f "${ci_workflow}" ]; then
  echo "required-gates validation must run from the repository root" >&2
  exit 1
fi

# ── Claim 1: declaration ─────────────────────────────────────────────────────

contexts=()
while IFS= read -r context; do
  contexts+=("${context}")
done < <(jq -r '
  .rules[]
  | select(.type == "required_status_checks")
  | .parameters.required_status_checks[]?.context
' "${config}")

if [ "${#contexts[@]}" -eq 0 ]; then
  echo "required-gates policy declares no required checks" >&2
  exit 1
fi

if [ "$(printf '%s\n' "${contexts[@]}" | sort -u | wc -l | tr -d ' ')" -ne "${#contexts[@]}" ]; then
  echo "required-gates policy declares duplicate check contexts" >&2
  exit 1
fi

for context in "${contexts[@]}"; do
  if grep -Fq "name: ${context}" "${ci_workflow}"; then
    continue
  fi

  # Anchored: an unanchored `title: ${title}` is a SUBSTRING match, and
  # `comment-section-title: Test` satisfied it. That made the declaration check
  # for `homeboy / Test` vacuous — it passed with the Test job repointed at a
  # foreign workflow and no longer running `review test` at all (#10997).
  title="${context#homeboy / }"
  if grep -Fq 'name: homeboy / ${{ matrix.title }}' "${ci_workflow}" \
    && grep -Eq "^[[:space:]]+title: ${title}[[:space:]]*$" "${ci_workflow}"; then
    continue
  fi

  # `homeboy / Test` is the caller job name plus the called reconciliation job
  # name, so no literal `name: homeboy / Test` exists to match. Accept the
  # reusable-workflow call instead — at a floating major OR a pinned commit SHA.
  # This branch previously required `@v2` and had therefore been dead since the
  # pin became a full SHA, which is why the broken title match above was the
  # only thing keeping this context green.
  if [ "${context}" = "homeboy / Test" ] \
    && grep -Eq 'uses: Extra-Chill/homeboy-action/\.github/workflows/ci\.yml@(v[0-9]+|[0-9a-f]{40})' "${ci_workflow}" \
    && grep -Fq 'commands: review test' "${ci_workflow}"; then
    continue
  fi

  {
    echo "required check '${context}' is not emitted by ${ci_workflow}" >&2
    exit 1
  }
done

if ! jq -e '
  .rules[]
  | select(.type == "required_status_checks")
  | .parameters.strict_required_status_checks_policy == true
' "${config}" >/dev/null; then
  echo "required-gates policy must require checks on the current PR head" >&2
  exit 1
fi

# ── Claim 1b: the declaration must reach a TERMINAL execution gate (#12573) ───
#
# Emitting a context is not running it. Every gate in `ci.yml` is conditional on
# `pr-state`, and a `skipped` needs-dependency does not fail a run, so a run that
# skipped all seven declared gates concluded `success` with this check green in
# the run before it and skipped in the run itself. The end-of-run assertion lives
# in `.github/ci-required-gates-executed.sh`; the part a pull request can break
# is its WIRING, so that is verified here from the checkout with no network.
#
# The terminal job must exist, must run under `always()` (a gate that skips
# alongside the gates it guards is not a gate), must invoke the execution
# assertion, and must depend on EVERY skippable gate job — otherwise a newly
# added gate could be skipped with nothing left to notice.

terminal_job='required-gates-executed'
terminal_section="$(awk -v key="  ${terminal_job}:" '
  $0 == key { inside = 1; next }
  inside && /^  [A-Za-z0-9_-]+:$/ { inside = 0 }
  inside { print }
' "${ci_workflow}")"

if [ -z "${terminal_section}" ]; then
  echo "${ci_workflow} declares no '${terminal_job}' job, so a run that skips every required gate can still conclude success" >&2
  exit 1
fi

for marker in \
  'name: homeboy / Required Gates Executed' \
  'if: ${{ always() }}' \
  'bash .github/ci-required-gates-executed.sh'; do
  if ! printf '%s\n' "${terminal_section}" | grep -Fq "${marker}"; then
    echo "the '${terminal_job}' job must contain '${marker}' to be a terminal gate" >&2
    exit 1
  fi
done

terminal_needs="$(printf '%s\n' "${terminal_section}" \
  | sed -n 's/^    needs: *//p' \
  | tr -d '[]' \
  | tr ',' ' ' \
  | tr -s ' ')"

if [ -z "${terminal_needs//[[:space:]]/}" ]; then
  echo "the '${terminal_job}' job must declare its dependencies as one inline 'needs: [...]' list" >&2
  exit 1
fi

# A job is skippable exactly when it carries the PR-state condition. Deriving the
# set from the workflow rather than hardcoding it is what makes this survive a
# gate being added: the new gate is covered the moment it is written.
gate_condition="if: \${{ needs.pr-state.outputs.active == 'true' }}"
gate_jobs="$(awk -v cond="${gate_condition}" '
  /^  [A-Za-z0-9_-]+:$/ { job = substr($0, 3, length($0) - 3); next }
  job != "" && index($0, cond) { print job; job = "" }
' "${ci_workflow}")"

if [ -z "${gate_jobs}" ]; then
  echo "${ci_workflow} declares no PR-state-conditional gate jobs, which contradicts the required-gates policy" >&2
  exit 1
fi

for gate_job in ${gate_jobs}; do
  case " ${terminal_needs} " in
    *" ${gate_job} "*) continue ;;
  esac

  echo "gate job '${gate_job}' is skippable but is not a dependency of '${terminal_job}', so skipping it would still conclude success" >&2
  exit 1
done

if [ "${mode}" = "--local" ]; then
  exit 0
fi

# ── Claim 2: enforcement ─────────────────────────────────────────────────────

repo="${GH_REPO:-Extra-Chill/homeboy}"
ruleset_id="${GH_RULESET_ID:-13680120}"
branch="${GH_TARGET_BRANCH:-main}"
# The checkout wins over `GITHUB_SHA`: on a `pull_request` event that variable
# is the synthetic merge commit, while `ci.yml` checks this job out at
# `pull_request.head.sha`. Evidence naming "the exact head SHA" should name the
# commit that was actually inspected.
head_sha="${REQUIRED_GATES_HEAD_SHA:-}"
if [ -z "${head_sha}" ]; then
  head_sha="$(git rev-parse HEAD 2>/dev/null || true)"
fi
if [ -z "${head_sha}" ]; then
  head_sha="${GITHUB_SHA:-unknown}"
fi

live_rules=''
live_ruleset=''
probe_error=''

# The effective-rules endpoint, not the ruleset endpoint, is the source of truth
# for "what actually gates this branch": it returns every active rule that
# applies regardless of the level it was configured at (repository OR
# organization), and it omits rulesets in `evaluate`/`disabled` enforcement. The
# ruleset endpoint below is read only for bypass evidence, and only a caller
# with write access gets `bypass_actors` back at all — hence its failure is
# tolerated rather than fatal.
read_live_rules() {
  local source="${REQUIRED_GATES_LIVE_RULES:-}"
  local payload=''

  if [ -n "${source}" ]; then
    if [ ! -f "${source}" ]; then
      probe_error="live-rules fixture ${source} does not exist"
      return 1
    fi
    payload="$(cat "${source}")"
  else
    if ! command -v gh >/dev/null 2>&1; then
      probe_error="gh is not installed, so live branch rules could not be read"
      return 1
    fi
    if ! payload="$(gh api "repos/${repo}/rules/branches/${branch}" 2>&1)"; then
      probe_error="gh api repos/${repo}/rules/branches/${branch} failed: $(printf '%s' "${payload}" | tr '\n' ' ')"
      return 1
    fi
  fi

  if ! jq -e 'type == "array"' >/dev/null 2>&1 <<< "${payload}"; then
    probe_error="live branch rules for ${branch} were not a JSON array"
    return 1
  fi

  live_rules="${payload}"
}

read_live_ruleset() {
  local source="${REQUIRED_GATES_LIVE_RULESET:-}"
  local payload=''

  if [ -n "${source}" ]; then
    [ -f "${source}" ] || return 1
    payload="$(cat "${source}")"
  else
    command -v gh >/dev/null 2>&1 || return 1
    payload="$(gh api "repos/${repo}/rulesets/${ruleset_id}" 2>/dev/null)" || return 1
  fi

  jq -e 'type == "object"' >/dev/null 2>&1 <<< "${payload}" || return 1
  live_ruleset="${payload}"
}

declared_contexts="$(jq -c '
  [.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]?.context]
  | sort
' "${config}")"
declared_count="${#contexts[@]}"

# Every live field starts at `unknown`, not at a plausible-looking zero. A
# report that cannot read GitHub must not print `live=0 strict=false`, which a
# reader would take for a measured absence rather than an absent measurement.
live_contexts='unknown'
live_count='unknown'
live_rule_count='unknown'
live_strict='unknown'
bypass_count='unknown'
bypass_current_user='unknown'
outcome='unverified'

if read_live_rules; then
  live_contexts="$(jq -c '
    [.[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]?.context]
    | sort
  ' <<< "${live_rules}")"
  live_count="$(jq 'length' <<< "${live_contexts}")"
  live_rule_count="$(jq '[.[] | select(.type == "required_status_checks")] | length' <<< "${live_rules}")"
  live_strict='false'
  if jq -e 'any(.[]; .type == "required_status_checks" and (.parameters.strict_required_status_checks_policy == true))' \
    >/dev/null <<< "${live_rules}"; then
    live_strict='true'
  fi

  if read_live_ruleset; then
    bypass_count="$(jq '(.bypass_actors // []) | length' <<< "${live_ruleset}")"
    bypass_current_user="$(jq -r '.current_user_can_bypass // "unknown"' <<< "${live_ruleset}")"
  fi

  if [ "${live_rule_count}" -eq 0 ]; then
    outcome='unenforced'
  elif [ "${live_contexts}" != "${declared_contexts}" ] || [ "${live_strict}" != 'true' ]; then
    outcome='divergent'
  elif { [ "${bypass_count}" != 'unknown' ] && [ "${bypass_count}" -gt 0 ]; } \
    || { [ "${bypass_current_user}" != 'unknown' ] && [ "${bypass_current_user}" != 'never' ]; }; then
    outcome='bypassable'
  else
    outcome='enforced'
  fi
fi

# One machine-readable provenance line in every branch, so a log reader can tell
# a verified enforcement from an unverified one without inferring it from the
# exit code — the same reason `release-quality-policy.sh` emits its measurement
# line before its verdict.
echo "::notice::required-gates enforcement basis=live-branch-rules repo=${repo} branch=${branch} ruleset=${ruleset_id} head=${head_sha} declared=${declared_count} live=${live_count} rules=${live_rule_count} strict=${live_strict} bypass_actors=${bypass_count} current_user_can_bypass=${bypass_current_user} outcome=${outcome}"

apply_hint="Apply .github/required-gates-ruleset.json to ruleset ${ruleset_id} (gh api --method PUT repos/${repo}/rulesets/${ruleset_id} --input .github/required-gates-ruleset.json), then verify with 'bash .github/validate-required-gates.sh --github'. See docs/operations/required-ci-gates.md."

case "${outcome}" in
  enforced)
    headline="GitHub requires all ${declared_count} declared contexts on ${branch}."
    echo "::notice::required-gates: ${headline}"
    ;;
  bypassable)
    headline="GitHub requires all ${declared_count} declared contexts on ${branch}, but the ruleset can be BYPASSED (bypass_actors=${bypass_count}, current_user_can_bypass=${bypass_current_user}). A merge by a bypass actor is not gated by these checks."
    echo "::warning::required-gates: ${headline}"
    ;;
  divergent)
    headline="GitHub's required checks on ${branch} DISAGREE with the declared policy. declared=${declared_contexts} live=${live_contexts} strict=${live_strict}. ${apply_hint}"
    echo "::warning::required-gates: ${headline}"
    ;;
  unenforced)
    headline="GitHub requires NO status checks on ${branch}. The ${declared_count} contexts in .github/required-gates-ruleset.json are declared but NOT enforced: a pull request can merge before any of them reports, exactly as PR #11069 did (#11084). This check verified the declaration only. ${apply_hint}"
    echo "::warning::required-gates: ${headline}"
    ;;
  unverified)
    headline="Live enforcement on ${branch} could NOT be verified (${probe_error}). This check verified the declaration only; treat enforcement as unproven, not as present. Re-run with a token that can read repository rules."
    echo "::warning::required-gates: ${headline}"
    ;;
esac

echo "required-gates-live-status=${outcome}"

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "live-status=${outcome}"
    echo "declared-contexts=${declared_contexts}"
    echo "live-contexts=${live_contexts}"
  } >> "${GITHUB_OUTPUT}"
fi

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "## Required gates"
    echo
    echo "| claim | result |"
    echo "| --- | --- |"
    echo "| declaration (\`ci.yml\` emits every declared context and wires every gate into \`required-gates-executed\`) | **verified** |"
    echo "| enforcement (GitHub requires them on \`${branch}\`) | **${outcome}** |"
    echo
    echo "${headline}"
    echo
    echo "| evidence | value |"
    echo "| --- | --- |"
    echo "| repository | \`${repo}\` |"
    echo "| target branch | \`${branch}\` |"
    echo "| ruleset id | \`${ruleset_id}\` |"
    echo "| head sha | \`${head_sha}\` |"
    echo "| declared contexts | \`${declared_contexts}\` |"
    echo "| live required contexts | \`${live_contexts}\` |"
    echo "| strict (require latest head) | \`${live_strict}\` |"
    echo "| bypass actors | \`${bypass_count}\` |"
    echo "| current user can bypass | \`${bypass_current_user}\` |"
  } >> "${GITHUB_STEP_SUMMARY}"
fi

if [ "${mode}" = "--report" ]; then
  # Reporting must never become enforcement (#11084). The declaration check
  # above already failed closed; the enforcement outcome is evidence only, so no
  # pull request becomes newly blocked by anything below this line.
  exit 0
fi

case "${outcome}" in
  enforced)
    exit 0
    ;;
  *)
    echo "required-gates enforcement is ${outcome} for ${repo}@${branch}" >&2
    exit 1
    ;;
esac
