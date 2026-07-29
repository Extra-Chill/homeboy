#!/usr/bin/env bash
#
# Release-blocking quality policy.
#
# Reads the three quality gate results and exits non-zero when a gate that
# `BLOCKING_COMMANDS` declared release-blocking did not succeed.
#
# ── The measurement invariant, ported to shell (#10741 / #10685) ──
#
# This script used to render its verdict from a population it never checked for
# emptiness. `check_command` matched each of `audit`/`lint`/`test` against the
# configured set; anything that did not match took the "tracked but not
# release-blocking" branch and left `failed` at 0. If NO command matched, all
# three took that branch and the policy exited 0 having enforced nothing.
#
# Reproduced before the fix:
#
#   BLOCKING_COMMANDS="review tests,review lints" \
#     AUDIT_RESULT=failure LINT_RESULT=failure TEST_RESULT=failure \
#     bash .github/release-quality-policy.sh   # -> exit 0
#
# Blast radius, stated accurately: the *absent* input is already safe, because
# `release.yml` interpolates
# `${{ inputs.release_blocking_commands || 'review lint,review test' }}` and
# GitHub's `||` treats the empty string as falsy, so the automated push path can
# never reach here with an empty set. The reachable defect is a **malformed
# non-empty set** — a `workflow_dispatch` typo such as `review tests` — which is
# exactly the shape below that is now a hard error.
#
# The classification mirrors `Measurement::assess` in
# `crates/homeboy-engine-primitives/src/measurement.rs`, using the same three
# outcomes and the same vocabulary, so the shell gate layer and the Rust gate
# layer answer this question the same way. Four divergent local answers to this
# same question is the disease #10690 was treating; a fifth, in bash, would be
# more of it.
#
#   population - configured blocking entries that canonicalize to a real token.
#                Known independently of the matcher: it comes from splitting the
#                input string, not from asking the matcher what it found.
#   units      - checked commands that the matcher claimed.
#
#   population == 0              -> empty-population -> honest zero, warn, exit 0
#   units == 0 (population > 0)  -> contradicted     -> broken instrument, exit 1
#   any entry uncheckable        -> unenforceable    -> silently unenforced, exit 1
#   otherwise                    -> measured         -> enforce, exit `failed`
#
# `unknown` is deliberately NOT collapsed into `fail` here, and `contradicted`
# deliberately IS a hard error — the same split `engine.rs` makes. See
# `assess_measurement` below for why each branch chose what it chose.
#
# Env:
#   BLOCKING_COMMANDS - comma-separated commands that may block release
#                       preparation (e.g. "review lint,review test")
#   AUDIT_RESULT      - gate-audit result
#   LINT_RESULT       - gate-lint result
#   TEST_RESULT       - gate-test result

set -euo pipefail

# Commands this policy is able to check. An entry in BLOCKING_COMMANDS naming
# anything outside this set declares a blocking requirement that no measurement
# can ever satisfy, which is why it is refused rather than ignored.
CHECKABLE_COMMANDS='audit lint test'

# Canonical form of a configured entry: lowercased, whitespace removed, and the
# `review` verb stripped, so "review lint", "Review Lint" and "lint" are one
# token. An entry that canonicalizes to the empty string declares nothing and is
# not counted towards the population.
canonicalize() {
  local canonical
  canonical="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  printf '%s' "${canonical#review}"
}

configured_tokens=()
unmatched_tokens=()

read_configured_tokens() {
  local configured
  local canonical
  local raw=()

  IFS=',' read -r -a raw <<< "${BLOCKING_COMMANDS}"
  for configured in ${raw+"${raw[@]}"}; do
    canonical="$(canonicalize "${configured}")"
    [ -n "${canonical}" ] || continue
    configured_tokens+=("${canonical}")
    case " ${CHECKABLE_COMMANDS} " in
      *" ${canonical} "*) ;;
      *) unmatched_tokens+=("${configured}") ;;
    esac
  done
}

is_blocking_command() {
  local command="$1"
  local token

  for token in ${configured_tokens+"${configured_tokens[@]}"}; do
    if [ "${token}" = "${command}" ]; then
      return 0
    fi
  done

  return 1
}

failed=0
measured_units=0

check_command() {
  local command="$1"
  local result="$2"

  if is_blocking_command "${command}"; then
    measured_units=$((measured_units + 1))
    if [ "${result}" = "success" ]; then
      echo "::notice::Release-blocking command ${command} passed"
    else
      echo "::error::Release-blocking command ${command} finished with result: ${result}"
      failed=1
    fi
  else
    echo "::notice::Command ${command} is tracked but not release-blocking (result: ${result})"
  fi
}

# The predicate. ONE function, deliberately.
#
# The first draft of this fix had two — `assess_measurement` for the empty
# matched population and `assess_configuration` for an entry naming an
# uncheckable command — and mutation testing showed why that was wrong:
# reverting the `contradicted` branch to a pass changed no observable
# behaviour, because zero matches against a non-empty population means *every*
# entry is unmatched, so the second guard always fired first. The load-bearing
# check was one of the two and the other was decoration that could rot
# untested. Two functions answering one question is the shape #10690 exists to
# stop; it does not become acceptable because both of them are mine.
#
# Classifies BEFORE the exit code is read, and emits one machine-readable
# provenance line in every branch so a log reader can tell a *measured zero*
# from *no measurement* without inferring it from the exit code. Absence of the
# line means the script died before assessing — a third state again, and also
# not a pass.
assess_measurement() {
  local population="${#configured_tokens[@]}"
  local unmatched="${#unmatched_tokens[@]}"
  local outcome

  if [ "${population}" -eq 0 ]; then
    outcome='empty-population'
  elif [ "${measured_units}" -eq 0 ]; then
    outcome='contradicted'
  elif [ "${unmatched}" -gt 0 ]; then
    outcome='unenforceable'
  else
    outcome='measured'
  fi

  echo "::notice::measurement basis=release-quality-policy population=${population} units=${measured_units} unmatched=${unmatched} outcome=${outcome}"

  case "${outcome}" in
    measured)
      return 0
      ;;
    empty-population)
      # A MEASURED zero. Splitting the configured string is independent of the
      # matcher, and it says there was genuinely nothing declared blocking, so
      # zero matches is the right answer rather than a broken matcher. That is
      # `MeasurementOutcome::EmptyPopulation`, which permits a pass.
      #
      # Warned rather than failed on purpose. `release.yml` cannot reach this
      # branch (the `||` fallback supplies the default set), so hard-failing it
      # would add red that no current run can produce while breaking any future
      # deliberate "block on nothing" configuration. Loud is enough.
      echo "::warning::No release-blocking commands are configured (BLOCKING_COMMANDS='${BLOCKING_COMMANDS}'), so this policy enforced nothing. Gate results are advisory for this run."
      return 0
      ;;
    contradicted)
      # `MeasurementOutcome::Contradicted`: zero matches against an
      # independently non-empty population. The matcher is provably broken, not
      # the gates, so neither `pass` nor `unknown` is honest — this is the one
      # outcome the shared predicate makes a hard error rather than a verdict.
      # This is the #10741 defect exactly.
      echo "::error::Release-blocking policy matched none of its ${population} configured command(s) (BLOCKING_COMMANDS='${BLOCKING_COMMANDS}'). Every gate fell through to the non-blocking branch, so this policy would have exited 0 while enforcing nothing. Checkable commands are: ${CHECKABLE_COMMANDS}."
      return 1
      ;;
    unenforceable)
      # The partial form of the same defect, and the subtler one: a single typo
      # in "review lint,review tests" leaves `units` non-zero, so the emptiness
      # check alone reads it as a healthy measurement while the test gate
      # silently stops blocking. Refused rather than warned because the remedy
      # is a one-character edit to a `workflow_dispatch` input, and because the
      # automated push path — whose set is the well-formed default — cannot
      # reach it.
      echo "::error::Release-blocking policy cannot check: ${unmatched_tokens[*]}. These entries of BLOCKING_COMMANDS='${BLOCKING_COMMANDS}' name no command this policy measures, so they declare a release-blocking requirement that is silently never enforced. Checkable commands are: ${CHECKABLE_COMMANDS}."
      return 1
      ;;
  esac
}

read_configured_tokens

check_command audit "${AUDIT_RESULT}"
check_command lint "${LINT_RESULT}"
check_command test "${TEST_RESULT}"

# The measurement is assessed before the gate verdict is returned, so a broken
# matcher can never be reported as a clean run.
assess_measurement || failed=1

exit "${failed}"
