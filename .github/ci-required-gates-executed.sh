#!/usr/bin/env bash
#
# Terminal gate: claim 3, EXECUTION (#12573).
#
# ── Why a third claim ──
#
# `validate-required-gates.sh` answers two questions, and both are evaluated at
# the START of a run:
#
#   declaration - every gate declared by `.github/required-gates-manifest.json`
#                 is emitted by its declared producer in `ci.yml`.
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
# That the required gates executed and passed in THIS run, measured from three
# directions so none can be satisfied vacuously:
#
#   typed logical results - every non-terminal gate's declared producer job,
#                        keyed by JOB ID from the manifest, concluded `success`
#                        in this run's `needs` context. GitHub types these
#                        results itself (matrix legs and reusable-workflow jobs
#                        arrive pre-aggregated), so no job name is
#                        reverse-engineered here (#13125). A producer missing
#                        from `needs` is wiring drift and fails closed.
#   dependency results - every dependency, not only the declared set,
#                        concluded `success`. `warning-clean` and
#                        `ci-capacity-admission` are not required contexts, but
#                        they carry the same `pr-state` condition as the gates
#                        that are, so their state is evidence about whether this
#                        run ran anything at all.
#   observed execution - every non-terminal context the manifest requires
#                        appears in this run's job list with conclusion
#                        `success`. The terminal job cannot observe its own
#                        conclusion while it runs; GitHub enforces that final
#                        required context itself. This is the claim the other
#                        directions cannot make: a context can be renamed away
#                        from its required name while its job still runs green
#                        (#10997's vacuous-declaration shape), or a job the
#                        whole run skipped (#12573's shape).
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
#   executed    every declared context ran and passed; every dependency and
#               every typed producer result concluded success. The only outcome
#               that exits 0.
#   skipped     at least one required gate did not execute at all.
#   failed      the gates executed and at least one did not pass.
#   unverified  this run's job list could not be read. Fails closed: an
#               unmeasured run is not a verified one.
#
# A policy that declares ZERO status checks is first-class: the manifest's
# zero-context policy names the outcome this script then reports, so its
# deliberate absence is never read as silence or failure (#13122).
#
# ── Fixture hooks ──
#
# REQUIRED_GATES_MANIFEST overrides the policy manifest;
# REQUIRED_GATES_EXECUTED_JOBS substitutes a jobs-API payload file for the live
# read; REQUIRED_GATES_HEAD_SHA identifies the candidate the jobs must measure;
# CI_GATE_RESULTS carries `toJSON(needs)`. All make this runnable without a
# network or a token.

set -euo pipefail

manifest="${REQUIRED_GATES_MANIFEST:-.github/required-gates-manifest.json}"
active="${CI_RUN_ACTIVE:-unknown}"
head_sha="${REQUIRED_GATES_HEAD_SHA:-${GITHUB_SHA:-}}"

if [ ! -f "${manifest}" ]; then
  echo "::error::required-gates execution check must run from the repository root"
  echo "required-gates execution check must run from the repository root" >&2
  exit 1
fi

if [ -z "${CI_GATE_RESULTS:-}" ]; then
  echo "::error::required-gates execution check requires CI_GATE_RESULTS=toJSON(needs)"
  echo "required-gates execution check requires CI_GATE_RESULTS=toJSON(needs)" >&2
  exit 1
fi

if [ -z "${head_sha}" ]; then
  echo "::error::required-gates execution check requires REQUIRED_GATES_HEAD_SHA"
  echo "required-gates execution check requires REQUIRED_GATES_HEAD_SHA" >&2
  exit 1
fi

if ! jq -e 'type == "object"' >/dev/null 2>&1 <<< "${CI_GATE_RESULTS}"; then
  echo "::error::CI_GATE_RESULTS is not the JSON object GitHub's needs context produces"
  echo "CI_GATE_RESULTS is not the JSON object GitHub's needs context produces" >&2
  exit 1
fi

zero_outcome="$(jq -r '.status_checks.zero_context_policy.execution_outcome' "${manifest}")"
zero_basis="$(jq -r '.status_checks.zero_context_policy.execution_basis' "${manifest}")"

declared_contexts="$(jq -c '
  [.gates[] | select(.required_status_check == true) | .context]
  | sort
' "${manifest}")"
declared_count="$(jq 'length' <<< "${declared_contexts}")"
execution_contexts="$(jq -c '
  [.gates[] | select(.required_status_check == true and .terminal != true) | .context]
  | sort
' "${manifest}")"
typed_gates="$(jq -c '
  [.gates[] | select(.required_status_check == true and .terminal != true)
   | {id, job: .producer.job}]
' "${manifest}")"

if [ "${declared_count}" -eq 0 ]; then
  # The manifest's zero-context policy is a declared reporting-only stance, not
  # an absent field. There is no execution claim to verify, so report exactly
  # the outcome the manifest names for this state.
  echo "::notice::required-gates-executed basis=${zero_basis} run=${GITHUB_RUN_ID:-unknown} attempt=${GITHUB_RUN_ATTEMPT:-unknown} pr_state_active=${active} declared=0 executed=0 outcome=${zero_outcome}"
  echo "::notice::required-gates-executed: The policy declares no required gates; execution reporting is not required."
  echo "required-gates-executed-status=${zero_outcome}"
  exit 0
fi

# ── Direction 1a: typed logical gate results (#13125) ────────────────────────
#
# Keyed by the manifest's producer JOB ID, which GitHub types in the needs
# context — matrix legs and reusable-workflow calls arrive pre-aggregated, so
# nothing here matches display names. A producer absent from `needs` is wiring
# drift: nothing executed for that gate, so it lands in the `skipped` class.

typed_summary="$(jq -c --argjson gates "${typed_gates}" '
  . as $needs
  | [ $gates[]
      | (.job) as $job
      | ($needs[$job].result // "missing-from-needs") as $result
      | {gate: .id, job: $job, result: $result} ]
' <<< "${CI_GATE_RESULTS}")"

typed_gate_failures="$(jq -r '
  [.[] | select(.result != "success") | "\(.gate)/\(.job)=\(.result)"] | join(" ")
' <<< "${typed_summary}")"
typed_skipped="$(jq -r '
  [.[] | select(.result == "skipped" or .result == "missing-from-needs") | "\(.gate)/\(.job)=\(.result)"] | join(" ")
' <<< "${typed_summary}")"
typed_failed="$(jq -r '
  [.[] | select(.result != "success" and .result != "skipped" and .result != "missing-from-needs") | "\(.gate)/\(.job)=\(.result)"] | join(" ")
' <<< "${typed_summary}")"

# ── Direction 1b: dependency results ─────────────────────────────────────────

dependency_failures="$(jq -r '
  to_entries
  | map(select(.value.result != "success") | "\(.key)=\(.value.result // "unknown")")
  | join(" ")
' <<< "${CI_GATE_RESULTS}")"
dependency_count="$(jq 'length' <<< "${CI_GATE_RESULTS}")"
dependency_skipped="$(jq '[.[] | select(.result == "skipped")] | length' <<< "${CI_GATE_RESULTS}")"

# ── Direction 1c: observed execution ─────────────────────────────────────────

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
  # the verdict wanted: the context produced no measurement. Jobs are scoped to
  # the PR head because a matching context from another candidate is not proof
  # that this candidate executed.
  #
  # Duplicate handling follows the manifest's declared aggregation semantics:
  # a workflow can emit a skipped planning job with the same display context as
  # its successful aggregate job, so success requires ANY success and NO hard
  # failure; every other combination fails closed (#13114).
  execution="$(jq -c --argjson required "${execution_contexts}" --arg head_sha "${head_sha}" '
    . as $jobs
    | [ $required[]
        | . as $context
        | ($jobs | map(select(.name == $context and .head_sha == $head_sha))) as $matches
        | {
            context: $context,
            head_sha: $head_sha,
            raw_conclusions: ($matches | map(.conclusion // "in-progress")),
            state: (
              if ($matches | length) == 0 then "not-executed"
              elif ($matches | any(.conclusion == "success"))
                and ($matches | all(.conclusion == "success" or .conclusion == "skipped"))
                then "success"
              elif ($matches | any(.conclusion != "success"))
                then ($matches | map(.conclusion // "in-progress") | unique | join(","))
              else "success"
              end
            ),
            selected_conclusion: (
              if ($matches | length) == 0 then "not-executed"
              elif ($matches | any(.conclusion == "success"))
                and ($matches | all(.conclusion == "success" or .conclusion == "skipped"))
                then "success"
              elif ($matches | any(.conclusion != "success" and .conclusion != "skipped"))
                then ($matches | map(select(.conclusion != "success" and .conclusion != "skipped") | .conclusion // "in-progress") | unique | join(","))
              else "skipped"
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
  # This terminal job is executing by definition. Its conclusion is enforced by
  # GitHub after this script exits, so it is not part of the jobs observation.
  executed_count="$(( $(jq '[.[] | select(.state != "not-executed")] | length' <<< "${execution}") + 1 ))"
fi

if [ "${execution}" = 'unknown' ]; then
  outcome='unverified'
elif [ -n "${not_executed}" ] || [ "${dependency_skipped}" -gt 0 ] || [ -n "${typed_skipped}" ]; then
  outcome='skipped'
elif [ -n "${not_passing}" ] || [ -n "${dependency_failures}" ] || [ -n "${typed_failed}" ]; then
  outcome='failed'
else
  outcome='executed'
fi

# One machine-readable provenance line in every branch, so a log reader can tell
# a verified execution from an unverified one without inferring it from the exit
# code — the same reason `validate-required-gates.sh` emits its enforcement basis
# before its verdict.
echo "::notice::required-gates-executed basis=typed-needs+needs-results+run-jobs run=${GITHUB_RUN_ID:-unknown} attempt=${GITHUB_RUN_ATTEMPT:-unknown} pr_state_active=${active} declared=${declared_count} executed=${executed_count} dependencies=${dependency_count} dependencies_skipped=${dependency_skipped} typed=[${typed_gate_failures}] outcome=${outcome}"
if [ "${execution}" != 'unknown' ]; then
  echo "::notice::required-gates-executed contexts=$(jq -c '.' <<< "${execution}")"
fi

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
    headline="Required gates did NOT all execute in this run. not-executed=[${not_executed}] skipped-dependencies=${dependency_skipped} typed=[${typed_gate_failures}] dependency-results=[${dependency_failures}]. Nothing was verified for these contexts, so this run must not read as success (#12573).${closure_note}"
    echo "::error::required-gates-executed: ${headline}"
    ;;
  failed)
    headline="Required gates executed and did not all pass. failing-contexts=[${not_passing}] typed=[${typed_gate_failures}] dependency-results=[${dependency_failures}]."
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
    echo "| logical gate | producer (typed needs) |"
    echo "| --- | --- |"
    jq -rn --argjson gates "${typed_gates}" --argjson needs "$(jq -S '.' <<< "${CI_GATE_RESULTS}")" '
      $gates[]
      | ($needs[.job].result // "missing-from-needs") as $result
      | "| `\(.id)` | `\(.job)` → \($result) |"
    '
    echo
    echo "| context | head SHA | raw conclusions | selected result | state |"
    echo "| --- | --- | --- | --- | --- |"
    if [ "${execution}" = 'unknown' ]; then
      jq -r '.[] | "| `\(.)` | `\(env.REQUIRED_GATES_HEAD_SHA)` | `unverified` | `unverified` | `unverified` |"' <<< "${declared_contexts}"
    else
      jq -r '.[] | "| `\(.context)` | `\(.head_sha)` | `\(.raw_conclusions | join(","))` | `\(.selected_conclusion)` | `\(.state)` |"' <<< "${execution}"
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
