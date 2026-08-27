#!/usr/bin/env bash
#
# Derive every required-gates artifact from the one policy manifest.
#
# `.github/required-gates-manifest.json` is the single declaration of which CI
# checks gate merges to `main` (#13125). This script validates that manifest and
# regenerates everything downstream of it:
#
#   .github/required-gates-ruleset.json   the live-ruleset payload, readable by
#                                         `gh api --input` exactly as before
#   docs/operations/required-ci-gates.md  the declared-gates table between the
#                                         GENERATED markers
#
# Nothing else in the repository may carry the gate list. The validator runs
# this script with --check, so a PR that edits either derived artifact without
# regenerating - or edits a gate anywhere but the manifest - turns
# `homeboy / Required Gates Declaration` red.
#
# ── Modes ──
#
#   write    regenerate the artifacts in place (default).
#   --check  regenerate into a scratch directory and compare bytes; exit 1 with
#            a diff if any artifact drifted from the manifest. Never writes.
#
# ── Fixture hooks ──
#
# REQUIRED_GATES_MANIFEST overrides the input manifest;
# REQUIRED_GATES_RULESET_OUTPUT / REQUIRED_GATES_DOCS_OUTPUT override the output
# paths, so tests can generate from a mutated manifest without touching the
# working tree.

set -euo pipefail

mode="${1:-write}"

case "${mode}" in
  write | --check) ;;
  *)
    echo "usage: bash .github/generate-required-gates-artifacts.sh [--check]" >&2
    exit 2
    ;;
esac

manifest="${REQUIRED_GATES_MANIFEST:-.github/required-gates-manifest.json}"
ruleset_output="${REQUIRED_GATES_RULESET_OUTPUT:-.github/required-gates-ruleset.json}"
# An empty REQUIRED_GATES_DOCS_OUTPUT skips the documentation artifact entirely,
# so tests can generate from a mutated manifest without a marked document.
docs_output="${REQUIRED_GATES_DOCS_OUTPUT-docs/operations/required-ci-gates.md}"
begin_marker='BEGIN GENERATED: required-gate-manifest'
end_marker='END GENERATED: required-gate-manifest'

if [ ! -f "${manifest}" ]; then
  echo "required-gates manifest ${manifest} is missing" >&2
  exit 1
fi

# ── Manifest validation: reject ambiguity at declaration time (#13125) ───────
#
# Duplicate GitHub contexts must be rejected here, before any execution is
# interpreted: two gates claiming one context makes both the ruleset payload
# and the execution aggregation ambiguous.
#
# Emptiness is refused for the same reason. Every artifact below is DERIVED from
# `.gates`, so a manifest declaring nothing derives a ruleset that requires
# nothing, and `--check` would then faithfully report that as current. That is
# #12833 -- a main ruleset enforcing zero status checks -- reached through
# generation rather than through live drift, and it must fail at declaration
# time rather than be discovered after a PR merges ahead of a red gate. A
# terminal gate is required for the same reason: it is the aggregate that proves
# the others actually executed, and a policy that quietly loses it keeps every
# individual context while losing the proof that any of them ran.

jq -e '
  (.schema == "homeboy/required-gates/v1") and
  (.gates | type == "array") and
  ((.gates | length) > 0) and
  ([.gates[].id] | (length == (unique | length))) and
  ([.gates[].context] | (length == (unique | length))) and
  ([.gates[] | select(.terminal == true)] | length == 1) and
  all(.gates[];
    (.id | type == "string" and length > 0) and
    (.context | type == "string" and startswith("homeboy / ")) and
    (.producer.workflow | type == "string") and
    (.producer.job | type == "string" and length > 0) and
    (.producer.emission == "job_name" or
     .producer.emission == "matrix_title" or
     .producer.emission == "reusable_workflow_call") and
    ((.aggregation == "single_job" and .producer.emission == "job_name") or
     (.aggregation == "matrix_legs" and .producer.emission == "matrix_title") or
     (.aggregation == "reusable_workflow_jobs" and .producer.emission == "reusable_workflow_call")) and
    ((.producer.emission != "matrix_title") or (.producer.matrix_title | type == "string" and length > 0)) and
    ((.producer.emission != "reusable_workflow_call") or
      ((.producer.called_workflow | type == "string" and length > 0) and
       (.producer.called_job_name | type == "string" and length > 0))) and
    (.required_status_check | type == "boolean")
  ) and
  all(.gates[] | select(.terminal == true);
    .required_status_check == true)
' "${manifest}" >/dev/null || {
  echo "required-gates manifest ${manifest} is structurally invalid (no gates, no single terminal gate, duplicate ids, duplicate contexts, unknown emission/aggregation kinds, or a non-required terminal gate)" >&2
  exit 1
}

tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir}"' EXIT

generate_ruleset() {
  jq '
    (.status_checks.integration_id) as $integration_id
    | {
        name: .ruleset_projection.name,
        target: .ruleset_projection.target,
        enforcement: .ruleset_projection.enforcement,
        bypass_actors: (.ruleset_projection.bypass_actors // []),
        conditions: .ruleset_projection.conditions,
        rules: ((.ruleset_projection.base_rules // []) +
          (if ([.gates[] | select(.required_status_check == true)] | length) > 0 then
            [{
              type: "required_status_checks",
              parameters: {
                do_not_enforce_on_create: .status_checks.do_not_enforce_on_create,
                required_status_checks: [
                  .gates[]
                  | select(.required_status_check == true)
                  | {context, integration_id: $integration_id}
                ],
                strict_required_status_checks_policy: .status_checks.strict_required_status_checks_policy
              }
            }]
          else [] end))
      }
  ' "${manifest}"
}

generate_docs_table() {
  jq -r '
    "| logical id | GitHub context | producer | status check |",
    "| --- | --- | --- | --- |",
    (.gates[] |
      (if .required_status_check == true then "required" else "declared-only" end) as $enforcement
      | (if .producer.emission == "matrix_title" then
          "`\(.producer.job)` > `\(.producer.matrix_title)`"
        elif .producer.emission == "reusable_workflow_call" then
          "`\(.producer.job)` -> `\(.producer.called_workflow)` (`\(.producer.called_job_name)`)"
        else
          "`\(.producer.job)`"
        end) as $producer
      | "| `\(.id)` | `\(.context)` | \($producer) | \($enforcement) |")
  ' "${manifest}"
}

generate_ruleset > "${tmpdir}/required-gates-ruleset.json"

if [ -n "${docs_output}" ]; then
  if [ ! -f "${docs_output}" ]; then
    echo "required-gates documentation ${docs_output} is missing" >&2
    exit 1
  fi
  if ! grep -Fq "<!-- ${begin_marker}" "${docs_output}" || ! grep -Fq "<!-- ${end_marker}" "${docs_output}"; then
    echo "${docs_output} is missing the ${begin_marker} / ${end_marker} markers" >&2
    exit 1
  fi

  {
    echo "<!-- ${begin_marker}; source of truth: .github/required-gates-manifest.json. Regenerate with .github/generate-required-gates-artifacts.sh. -->"
    generate_docs_table
  } > "${tmpdir}/declared-gates-table.md"

  # Assemble deterministically: everything before the BEGIN marker, the generated
  # table, then everything from the END marker line onward.
  awk -v table="${tmpdir}/declared-gates-table.md" -v begin="<!-- ${begin_marker}" -v end="<!-- ${end_marker}" '
    function emit_table() {
      while ((getline line < table) > 0) print line
      close(table)
    }
    !done && index($0, begin) == 1 { emit_table(); done = 1; next }
    done && index($0, end) == 1 { in_tail = 1 }
    !done || in_tail { print }
  ' "${docs_output}" > "${tmpdir}/required-ci-gates.md"
fi

if [ "${mode}" = "--check" ]; then
  drift=''
  cmp -s "${tmpdir}/required-gates-ruleset.json" "${ruleset_output}" || drift="${drift} ${ruleset_output}"
  if [ -n "${docs_output}" ]; then
    cmp -s "${tmpdir}/required-ci-gates.md" "${docs_output}" || drift="${drift} ${docs_output}"
  fi
  if [ -n "${drift}" ]; then
    echo "generated required-gates artifacts drifted from ${manifest}:${drift}" >&2
    echo "run 'bash .github/generate-required-gates-artifacts.sh' and commit the regenerated artifacts" >&2
    diff -u "${ruleset_output}" "${tmpdir}/required-gates-ruleset.json" | sed -n '3,12p' >&2 || true
    exit 1
  fi
  echo "::notice::required-gates generation basis=manifest manifest=${manifest} outcome=current"
  exit 0
fi

jq -e 'type == "object"' >/dev/null <<< "$(cat "${tmpdir}/required-gates-ruleset.json")" || {
  echo "generated ruleset payload is not a JSON object" >&2
  exit 1
}

cp "${tmpdir}/required-gates-ruleset.json" "${ruleset_output}"
if [ -n "${docs_output}" ]; then
  cp "${tmpdir}/required-ci-gates.md" "${docs_output}"
fi
echo "::notice::required-gates generation basis=manifest manifest=${manifest} outcome=regenerated"
