#!/usr/bin/env bash
#
# Terminal gate: claim 3, EXECUTION (#12573).
#
# ── Why a third claim ──
#
# `validate-required-gates.sh` answers two questions, and both are evaluated at
# the START of a run:
#
#   declaration - `ci.yml` emits every context named by the versioned payload.
#   enforcement - GitHub actually requires those contexts before `main` moves.
#
# Neither can answer the question PR #12567 needed answered: did the gates then
# RUN? Run 31906427396 was cancelled with every gate mid-flight. The run that
# cancelled it, 31906482704, was the `pull_request.closed` run, so `pr-state`
# reported inactive, every gate skipped, and the workflow concluded SUCCESS
# having compiled nothing and tested nothing. `Required Gates Declaration` was
# green in the first run while declaring seven gates that never finished, and
# skipped in the second while declaring seven gates that never started.
#
# A `skipped` needs-dependency does not fail a run, so "nothing ran" and
# "everything passed" produced the same green tick. `gh pr checks` renders the
# superseded run's `cancelled` jobs as `fail`, so the pull request showed a wall
# of red and a green overall conclusion at the same time. Neither was true.
#
# ── What this script claims ──
#
# That the required gates executed and passed in THIS run, measured from two
# independent directions so neither can be satisfied vacuously:
#
#   dependency results - every gate job `ci.yml` names as a dependency concluded
#                        `success`. Tokenless, structural, and here `skipped`
#                        and `cancelled` are failures rather than silence.
#   observed execution - every non-terminal context in
#                        `.github/required-gates-ruleset.json` appears in this
#                        run's job list with conclusion `success`. The terminal
#                        job cannot observe its own conclusion while it runs;
#                        GitHub enforces that final required context itself.
#                        This is the claim the declaration check
#                        cannot make: a context can be declared by a job that
#                        never ran (#10997's vacuous-declaration shape) or by a
#                        job the whole run skipped (#12573's shape).
#
# ── What this script deliberately does NOT do ──
#
# It does not change capacity admission, and it does not change the closure
# run's cancellation of an in-flight candidate DAG. Skipping stays possible; it
# stops being invisible. A closure run is red here on purpose: it is the LAST
# word on the pull request and it is the run that read green in #12573, so
# exempting it would leave the reported hole exactly as it was found.
#
# ── Outcomes ──
#
# Vocabulary mirrors `validate-required-gates.sh`, which already had to learn
# that "the check passed" and "the check measured something" are different
# facts:
#
#   executed    every declared context ran and passed; every dependency
#               concluded success. The only outcome that exits 0.
#   skipped     at least one required gate did not execute at all.
#   failed      the gates executed and at least one did not pass.
#   unverified  this run's job list could not be read. Fails closed: an
#               unmeasured run is not a verified one.
#
# ── Fixture hooks ──
#
# REQUIRED_GATES_CONFIG overrides the declared-context payload;
# REQUIRED_GATES_EXECUTED_JOBS substitutes a jobs-API payload file for the live
# read; CI_GATE_RESULTS carries `toJSON(needs)`. All three make this runnable
# without a network or a token.

set -euo pipefail

config="${REQUIRED_GATES_CONFIG:-.github/required-gates-ruleset.json}"
active="${CI_RUN_ACTIVE:-unknown}"

if [ ! -f "${config}" ]; then
  echo "::error::required-gates execution check must run from the repository root"
  echo "required-gates execution check must run from the repository root" >&2
  exit 1
fi

if [ -z "${CI_GATE_RESULTS:-}" ]; then
  echo "::error::required-gates execution check requires CI_GATE_RESULTS=toJSON(needs)"
  echo "required-gates execution check requires CI_GATE_RESULTS=toJSON(needs)" >&2
  exit 1
fi

if ! jq -e 'type == "object"' >/dev/null 2>&1 <<< "${CI_GATE_RESULTS}"; then
  echo "::error::CI_GATE_RESULTS is not the JSON object GitHub's needs context produces"
  echo "CI_GATE_RESULTS is not the JSON object GitHub's needs context produces" >&2
  exit 1
fi

# ── Direction 1: dependency results ──────────────────────────────────────────
#
# Every dependency, not only the declared required set. `warning-clean` and
# `ci-capacity-admission` are not required contexts, but they carry the same
# `pr-state` condition as the gates that are, so their state is evidence about
# whether this run ran anything at all.

dependency_failures="$(jq -r '
  to_entries
  | map(select(.value.result != "success") | "\(.key)=\(.value.result // "unknown")")
  | join(" ")
' <<< "${CI_GATE_RESULTS}")"
dependency_count="$(jq 'length' <<< "${CI_GATE_RESULTS}")"
dependency_skipped="$(jq '[.[] | select(.result == "skipped")] | length' <<< "${CI_GATE_RESULTS}")"

# ── Direction 2: observed execution ──────────────────────────────────────────

declared_contexts="$(jq -c '
  [.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]?.context]
  | sort
' "${config}")"
declared_count="$(jq 'length' <<< "${declared_contexts}")"
execution_contexts="$(jq -c '[.[] | select(. != "homeboy / Required Gates Executed")]' <<< "${declared_contexts}")"

if [ "${declared_count}" -eq 0 ]; then
  echo "::error::required-gates policy declares no required checks, so execution cannot be verified"
  echo "required-gates policy declares no required checks" >&2
  exit 1
fi

run_jobs=''
probe_error=''

read_run_jobs() {
  local source="${REQUIRED_GATES_EXECUTED_JOBS:-}"
  local payload=''

  if [ -n "${source}" ]; then
    if [ ! -f "${source}" ]; then
      probe_error="run-jobs fixture ${source} does not exist"
      return 1
    fi
    payload="$(cat "${source}")"
  else
    if ! command -v gh >/dev/null 2>&1; then
      probe_error="gh is not installed, so this run's job list could not be read"
      return 1
    fi
    if ! payload="$(gh api --paginate \
      "repos/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}/jobs?per_page=100" \
      | jq -s '[.[] | .jobs[]]' 2>&1)"; then
      probe_error="gh api actions/runs/${GITHUB_RUN_ID:-} /jobs failed: $(printf '%s' "${payload}" | tr '\n' ' ')"
      return 1
    fi
  fi

  if ! jq -e 'type == "array"' >/dev/null 2>&1 <<< "${payload}"; then
    probe_error="this run's job list was not a JSON array"
    return 1
  fi

  run_jobs="${payload}"
}

execution='unknown'
not_executed=''
not_passing=''

if read_run_jobs; then
  # A skipped matrix job is reported under its UNEXPANDED name (#12573 observed
  # `${{ matrix.title }}`), so `homeboy / Audit` is simply absent rather than
  # present-and-skipped. Absent is therefore `not-executed`, which is exactly
  # the verdict wanted: the context produced no measurement.
  execution="$(jq -c --argjson required "${execution_contexts}" '
    . as $jobs
    | [ $required[]
        | . as $context
        | ($jobs | map(select(.name == $context))) as $matches
        | {
            context: $context,
            state: (
              if ($matches | length) == 0 then "not-executed"
              # The workflow can emit a skipped planning job with the same
              # display context as its successful aggregate job. Accept that
              # exact duplicate shape, but retain fail-closed handling for
              # every other non-success conclusion.
              elif ($matches | any(.conclusion == "success"))
                and ($matches | all(.conclusion == "success" or .conclusion == "skipped"))
                then "success"
              elif ($matches | any(.conclusion != "success"))
                then ($matches | map(.conclusion // "in-progress") | unique | join(","))
              else "success"
              end
            )
          }
      ]
  ' <<< "${run_jobs}")"
  not_executed="$(jq -r '[.[] | select(.state == "not-executed") | .context] | join(", ")' <<< "${execution}")"
  not_passing="$(jq -r '
    [.[] | select(.state != "success" and .state != "not-executed") | "\(.context)=\(.state)"]
    | join(", ")
  ' <<< "${execution}")"
fi

# ── Verdict ──────────────────────────────────────────────────────────────────
#
# `skipped` outranks `failed`: a gate that ran and failed is already a red check
# a human cannot miss, while a gate that never ran is the invisible case this
# job exists to surface.

executed_count='unknown'
if [ "${execution}" != 'unknown' ]; then
  executed_count="$(jq '[.[] | select(.state != "not-executed")] | length' <<< "${execution}")"
fi

if [ "${execution}" = 'unknown' ]; then
  outcome='unverified'
elif [ -n "${not_executed}" ] || [ "${dependency_skipped}" -gt 0 ]; then
  outcome='skipped'
elif [ -n "${not_passing}" ] || [ -n "${dependency_failures}" ]; then
  outcome='failed'
else
  outcome='executed'
fi

# One machine-readable provenance line in every branch, so a log reader can tell
# a verified execution from an unverified one without inferring it from the exit
# code — the same reason `validate-required-gates.sh` emits its enforcement basis
# before its verdict.
echo "::notice::required-gates-executed basis=needs-results+run-jobs run=${GITHUB_RUN_ID:-unknown} attempt=${GITHUB_RUN_ATTEMPT:-unknown} pr_state_active=${active} declared=${declared_count} executed=${executed_count} dependencies=${dependency_count} dependencies_skipped=${dependency_skipped} outcome=${outcome}"

closure_note=''
if [ "${active}" = 'false' ]; then
  closure_note=" This run is a pull_request.closed run: it exists to cancel the in-flight run in the same concurrency group, so it verified nothing. That is not a defect in this job — it is the state #12573 reported as a green tick, and it is now stated instead."
fi

case "${outcome}" in
  executed)
    headline="All ${declared_count} required gates executed and passed in this run."
    echo "::notice::required-gates-executed: ${headline}"
    ;;
  skipped)
    headline="Required gates did NOT all execute in this run. not-executed=[${not_executed}] skipped-dependencies=${dependency_skipped} dependency-results=[${dependency_failures}]. Nothing was verified for these contexts, so this run must not read as success (#12573).${closure_note}"
    echo "::error::required-gates-executed: ${headline}"
    ;;
  failed)
    headline="Required gates executed and did not all pass. failing-contexts=[${not_passing}] dependency-results=[${dependency_failures}]."
    echo "::error::required-gates-executed: ${headline}"
    ;;
  unverified)
    headline="This run's job list could NOT be read (${probe_error}), so gate execution is unproven. Failing closed: an unmeasured run is not a verified one."
    echo "::error::required-gates-executed: ${headline}"
    ;;
esac

echo "required-gates-executed-status=${outcome}"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "## Required gates executed"
    echo
    echo "| claim | result |"
    echo "| --- | --- |"
    echo "| execution (every declared context ran and passed in this run) | **${outcome}** |"
    echo
    echo "${headline}"
    echo
    echo "| context | state |"
    echo "| --- | --- |"
    if [ "${execution}" = 'unknown' ]; then
      jq -r '.[] | "| `\(.)` | `unverified` |"' <<< "${declared_contexts}"
    else
      jq -r '.[] | "| `\(.context)` | `\(.state)` |"' <<< "${execution}"
    fi
    echo
    echo "| dependency | result |"
    echo "| --- | --- |"
    jq -r 'to_entries[] | "| `\(.key)` | `\(.value.result // "unknown")` |"' <<< "${CI_GATE_RESULTS}"
  } >> "${GITHUB_STEP_SUMMARY}"
fi

if [ "${outcome}" = 'executed' ]; then
  exit 0
fi

echo "required-gates execution is ${outcome} for run ${GITHUB_RUN_ID:-unknown}" >&2
exit 1
