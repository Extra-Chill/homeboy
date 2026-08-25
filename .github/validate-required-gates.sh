#!/usr/bin/env bash
#
# Main-branch gate contract validator.
#
# ── One policy declaration (#13125) ──
#
# Everything this script checks derives from one versioned manifest,
# `.github/required-gates-manifest.json`. The manifest declares each gate's
# logical identity, emitted GitHub context, workflow/job producer, aggregation
# semantics, live-ruleset requirement, and the first-class zero-required-check
# policy. The generated ruleset payload and the documentation table are derived
# from it by `.github/generate-required-gates-artifacts.sh`; this script fails
# closed when any of them has drifted from the manifest.
#
# ── Two claims, not one (#11084) ──
#
# This script can answer two questions, and they are NOT the same question:
#
#   declaration - every gate declared by the manifest is emitted by its declared
#                 producer in `.github/workflows/ci.yml`, every skippable gate
#                 job is wired into the terminal execution gate, and the
#                 generated artifacts match the manifest. This is repository
#                 content, so a PR can both break it and fix it, and it is
#                 checked from the checkout with no network.
#   enforcement - GitHub actually *requires* those contexts before `main` can be
#                 updated. This is repository STATE. A pull request cannot
#                 change it and it can be false while the declaration is
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
#   not-required  the manifest declares zero status checks (its first-class
#             zero-context policy); live state agreeing is success, not absence.
#   unverified  live state could not be read. NEVER reported as enforced.
#
# ── Fixture hooks ──
#
# REQUIRED_GATES_MANIFEST / REQUIRED_GATES_WORKFLOW override the declaration
# inputs; REQUIRED_GATES_RULESET_OUTPUT / REQUIRED_GATES_DOCS_OUTPUT override
# where the generator expects the derived artifacts (so tests can validate a
# mutated manifest against consistently regenerated scratch artifacts);
# REQUIRED_GATES_LIVE_RULES / REQUIRED_GATES_LIVE_RULESET substitute a JSON file
# for the two live API reads; REQUIRED_GATES_HEAD_SHA pins the recorded head.
# `tests/required_gates_policy_test.rs` uses these to pin every outcome above
# without a network or a token.

set -euo pipefail

mode="${1:---local}"

case "${mode}" in
  --local | --report | --github) ;;
  *)
    echo "usage: bash .github/validate-required-gates.sh [--local|--report|--github]" >&2
    exit 2
    ;;
esac

manifest="${REQUIRED_GATES_MANIFEST:-.github/required-gates-manifest.json}"
ci_workflow="${REQUIRED_GATES_WORKFLOW:-.github/workflows/ci.yml}"
gate_condition="if: \${{ needs.pr-state.outputs.active == 'true' }}"

if [ ! -f "${manifest}" ] || [ ! -f "${ci_workflow}" ]; then
  echo "required-gates validation must run from the repository root" >&2
  exit 1
fi

# The whole body of a top-level job's YAML, keyed by job id.
job_section() {
  awk -v key="  ${1}:" '
    $0 == key { inside = 1; next }
    inside && /^  [A-Za-z0-9_-]+:$/ { inside = 0 }
    inside { print }
  ' "${ci_workflow}"
}

fail() {
  echo "$1" >&2
  exit 1
}

# ── Claim 0: the derived artifacts match the manifest ────────────────────────
#
# The ruleset payload and the documentation table are GENERATED. A PR that
# edits any of them — or a gate anywhere but the manifest — must fail here with
# regeneration instructions, not silently redefine policy at the leaves.

bash .github/generate-required-gates-artifacts.sh --check \
  || fail "required-gates declaration rejected: the manifest and its generated artifacts disagree"

# ── Claim 1: declaration ─────────────────────────────────────────────────────

required_count="$(jq '[.gates[] | select(.required_status_check == true)] | length' "${manifest}")"

while IFS= read -r gate; do
  gate_id="$(jq -r '.id' <<< "${gate}")"
  context="$(jq -r '.context' <<< "${gate}")"
  producer_job="$(jq -r '.producer.job' <<< "${gate}")"
  emission="$(jq -r '.producer.emission' <<< "${gate}")"

  section="$(job_section "${producer_job}")"
  if [ -z "${section}" ]; then
    fail "manifest gate '${gate_id}' declares producer job '${producer_job}', but ${ci_workflow} declares no such job"
  fi

  case "${emission}" in
    job_name)
      # Scoped to the declared producer's own YAML block. The old global
      # substring grep accepted an unrelated `comment-section-title: Test` as
      # proof that `homeboy / Test` was emitted (#10997); a scoped exact
      # `name:` line cannot be satisfied vacuously.
      printf '%s\n' "${section}" | grep -Fq "name: ${context}" \
        || fail "required check '${context}' is not emitted by producer job '${producer_job}' in ${ci_workflow}"
      ;;
    matrix_title)
      title="$(jq -r '.producer.matrix_title' <<< "${gate}")"
      if ! printf '%s\n' "${section}" | grep -Fq 'name: homeboy / ${{ matrix.title }}' \
        || ! printf '%s\n' "${section}" | grep -Eq "^[[:space:]]+title: ${title}[[:space:]]*$"; then
        fail "required check '${context}' is not emitted by the '${producer_job}' matrix (no leg titled '${title}') in ${ci_workflow}"
      fi
      ;;
    reusable_workflow_call)
      called_workflow="$(jq -r '.producer.called_workflow' <<< "${gate}")"
      # At a floating major OR a pinned commit SHA; annotated tags cannot be
      # resolved by `uses:` at all (#12153).
      printf '%s\n' "${section}" | grep -Eq "uses: ${called_workflow}@(v[0-9]+|[0-9a-f]{40})" \
        || fail "required check '${context}' is not emitted by the reusable workflow call in producer job '${producer_job}' in ${ci_workflow}"
      ;;
    *)
      fail "manifest gate '${gate_id}' declares unknown emission kind '${emission}'"
      ;;
  esac

  admission="$(jq -r '.admission_gate // empty' <<< "${gate}")"
  # The terminal gate is deliberately NOT pr-state conditional: it runs under
  # `always()` so a skipped run cannot skip its own verdict (#12573).
  if [ "${admission}" = "pr_state_active" ] && [ "$(jq -r '.terminal // false' <<< "${gate}")" != "true" ]; then
    printf '%s\n' "${section}" | grep -Fq "${gate_condition}" \
      || fail "manifest gate '${gate_id}' declares pr-state admission, but producer job '${producer_job}' does not carry the PR-state condition in ${ci_workflow}"
  fi
done < <(jq -c '.gates[]' "${manifest}")

if [ "${required_count}" -gt 0 ]; then
  if ! jq -e '.status_checks.strict_required_status_checks_policy == true' "${manifest}" >/dev/null; then
    fail "required-gates policy must require checks on the current PR head"
  fi
fi

# ── Claim 1b: the declaration must reach a TERMINAL execution gate (#12573) ───
#
# Emitting a context is not running it. Every gate in `ci.yml` is conditional on
# `pr-state`, and a `skipped` needs-dependency does not fail a run, so a run that
# skipped every declared gate concluded `success` with this check green in the
# run before it and skipped in the run itself. The end-of-run assertion lives
# in `.github/ci-required-gates-executed.sh`; the part a pull request can break
# is its WIRING, so that is verified here from the checkout with no network.

terminal_gate="$(jq -c '[.gates[] | select(.terminal == true)] | if length == 1 then .[0] else empty end' "${manifest}")"
if [ -z "${terminal_gate}" ]; then
  fail "the manifest declares exactly zero or multiple terminal gates, so a run that skips every required gate could still conclude success"
fi

terminal_script="$(jq -r '.execution_evidence.terminal_script' "${manifest}")"
terminal_job="$(jq -r '.producer.job' <<< "${terminal_gate}")"
terminal_context="$(jq -r '.context' <<< "${terminal_gate}")"

terminal_section="$(job_section "${terminal_job}")"
if [ -z "${terminal_section}" ]; then
  fail "${ci_workflow} declares no '${terminal_job}' job, so a run that skips every required gate can still conclude success"
fi

for marker in \
  "name: ${terminal_context}" \
  'if: ${{ always() }}' \
  "bash ${terminal_script}"; do
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
skippable_jobs="$(awk -v cond="${gate_condition}" '
  /^  [A-Za-z0-9_-]+:$/ { job = substr($0, 3, length($0) - 3); next }
  job != "" && index($0, cond) { print job; job = "" }
' "${ci_workflow}")"

if [ -z "${skippable_jobs}" ]; then
  echo "${ci_workflow} declares no PR-state-conditional gate jobs, which contradicts the required-gates policy" >&2
  exit 1
fi

for skippable in ${skippable_jobs}; do
  case " ${terminal_needs} " in
    *" ${skippable} "*) continue ;;
  esac

  echo "gate job '${skippable}' is skippable but is not a dependency of '${terminal_job}', so skipping it would still conclude success" >&2
  exit 1
done

# And conversely: every non-terminal gate the manifest declares must already be
# among the terminal job's dependencies, keyed by the SAME producer job id the
# terminal gate consumes typed needs results through.
while IFS= read -r gate; do
  jq -e '.terminal != true' >/dev/null 2>&1 <<< "${gate}" || continue
  producer_job="$(jq -r '.producer.job' <<< "${gate}")"
  case " ${terminal_needs} " in
    *" ${producer_job} "*) continue ;;
  esac
  echo "manifest gate '$(jq -r '.id' <<< "${gate}")' producer job '${producer_job}' is not a dependency of '${terminal_job}'" >&2
  exit 1
done < <(jq -c '.gates[]' "${manifest}")

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
  [.gates[] | select(.required_status_check == true) | .context]
  | sort
' "${manifest}")"
zero_outcome="$(jq -r '.status_checks.zero_context_policy.enforcement_outcome' "${manifest}")"

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
    if [ "${required_count}" -eq 0 ]; then
      # The manifest's first-class zero-context policy: a measured live zero
      # agrees with a declared zero. The outcome label itself comes from the
      # manifest, so even this verdict has no parallel literal.
      outcome="${zero_outcome}"
    else
      outcome='unenforced'
    fi
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
echo "::notice::required-gates enforcement basis=live-branch-rules repo=${repo} branch=${branch} ruleset=${ruleset_id} head=${head_sha} declared=${required_count} live=${live_count} rules=${live_rule_count} strict=${live_strict} bypass_actors=${bypass_count} current_user_can_bypass=${bypass_current_user} outcome=${outcome}"

apply_hint="Use the approved Required Gates Ruleset workflow from current main after its exact SHA has a successful homeboy / Test check, then verify with 'bash .github/validate-required-gates.sh --github'. See docs/operations/required-ci-gates.md."

case "${outcome}" in
  "${zero_outcome}")
    headline="The canonical policy and live rules require no status checks on ${branch}. CI remains reporting-only."
    echo "::notice::required-gates: ${headline}"
    ;;
  enforced)
    headline="GitHub requires all ${required_count} declared contexts on ${branch}."
    echo "::notice::required-gates: ${headline}"
    ;;
  bypassable)
    headline="GitHub requires all ${required_count} declared contexts on ${branch}, but the ruleset can be BYPASSED (bypass_actors=${bypass_count}, current_user_can_bypass=${bypass_current_user}). A merge by a bypass actor is not gated by these checks."
    echo "::warning::required-gates: ${headline}"
    ;;
  divergent)
    headline="GitHub's required checks on ${branch} DISAGREE with the declared policy. declared=${declared_contexts} live=${live_contexts} strict=${live_strict}. ${apply_hint}"
    echo "::warning::required-gates: ${headline}"
    ;;
  unenforced)
    headline="GitHub requires NO status checks on ${branch}. The ${required_count} contexts declared by .github/required-gates-manifest.json are declared but NOT enforced: a pull request can merge before any of them reports, exactly as PR #11069 did (#11084). This check verified the declaration only. ${apply_hint}"
    echo "::warning::required-gates: ${headline}"
    ;;
  unverified)
    headline="Live enforcement on ${branch} could NOT be verified (${probe_error}). This check verified the declaration only; treat enforcement as unproven, not as present. Re-run with a token that can read repository rules."
    echo "::warning::required-gates: ${headline}"
    ;;
  *)
    fail "internal error: unknown enforcement outcome '${outcome}'"
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
    echo "| manifest | \`${manifest}\` |"
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
  enforced | "${zero_outcome}")
    exit 0
    ;;
  *)
    echo "required-gates enforcement is ${outcome} for ${repo}@${branch}" >&2
    exit 1
    ;;
esac
