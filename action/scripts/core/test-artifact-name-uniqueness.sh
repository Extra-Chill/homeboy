#!/usr/bin/env bash

# Artifact names must be unique per workflow run.
#
# Regression guard for #483: two operations invocations in one job produced
# byte-identical artifact suffixes, so the second upload failed with
#
#   Failed to CreateArtifact: (409) Conflict: an artifact with this name
#   already exists on the workflow run
#
# and reported the whole job as failed even though every command in it
# succeeded. The cause was that `resolved-commands` — the only field meant to
# distinguish invocations — is empty for deploy/fleet, which route to
# `operations-commands` instead. This asserts the suffix distinguishes
# invocations by operations commands AND by the step's own instance id, so
# even two identical command lists in one job cannot collide.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fail=0
check() { if [ "$2" = "$3" ]; then echo "PASS: $1"; else echo "FAIL: $1 (expected '$3', got '$2')"; fail=1; fi; }
differs() { if [ "$2" != "$3" ]; then echo "PASS: $1"; else echo "FAIL: $1 (both '$2')"; fail=1; fi; }

# Mirror of the suffix construction in action.yml's "Resolve artifact names".
suffix() {
  local artifact_suffix="${1}-${2}-${3}-${4}-${5}-${6}"
  [ -n "${7:-}" ] && artifact_suffix="${artifact_suffix}-php${7}"
  [ -n "${8:-}" ] && artifact_suffix="${artifact_suffix}-node${8}"
  artifact_suffix="$(printf '%s' "${artifact_suffix}" | tr -cs 'A-Za-z0-9._-' '-')"
  artifact_suffix="${artifact_suffix#-}"
  artifact_suffix="${artifact_suffix%-}"
  [ -z "${artifact_suffix}" ] && artifact_suffix="${4:-homeboy}"
  printf '%s' "${artifact_suffix}"
}

# The exact #483 shape: same component, job, runner; deploy then verify.
# resolved-commands is empty for both because operations commands route
# elsewhere — this is what made the old suffix collapse.
a="$(suffix extrachill-network '' 'deploy extrachill-site data-machine-events --version 0.63.4' deploy __run Linux 8.4 24)"
b="$(suffix extrachill-network '' 'deploy extrachill-site --check' deploy __run_2 Linux 8.4 24)"
differs "deploy and verify invocations get distinct suffixes" "$a" "$b"

# Two invocations of the *same* command list must still differ: the instance
# id is what guarantees it, not the command text.
c="$(suffix extrachill-network '' 'fleet check prod' ops __run Linux '' '')"
d="$(suffix extrachill-network '' 'fleet check prod' ops __run_2 Linux '' '')"
differs "identical command lists in one job still differ by instance id" "$c" "$d"

# Quality invocations keep distinguishing on resolved-commands.
e="$(suffix my-plugin 'review lint' '' quality __run Linux 8.3 20)"
f="$(suffix my-plugin 'review test' '' quality __run_2 Linux 8.3 20)"
differs "quality invocations still differ by resolved commands" "$e" "$f"

# Sanity: the suffix is still a valid artifact name (no forbidden characters).
for s in "$a" "$b" "$c" "$d" "$e" "$f"; do
  case "$s" in
    *[!A-Za-z0-9._-]*) echo "FAIL: suffix contains an invalid character: $s"; fail=1 ;;
  esac
  [ -n "$s" ] || { echo "FAIL: empty suffix"; fail=1; }
done
echo "PASS: every suffix is a valid, non-empty artifact name"

# The action must actually feed both disambiguators into that construction.
grep -q 'OPERATIONS_COMMANDS_VALUE: ${{ steps.resolve-commands.outputs.operations-commands }}' "${ROOT_DIR}/action.yml" \
  && echo "PASS: action.yml passes operations-commands into the suffix" \
  || { echo "FAIL: action.yml does not pass operations-commands into the suffix"; fail=1; }
grep -q 'ACTION_INSTANCE_VALUE: ${{ github.action }}' "${ROOT_DIR}/action.yml" \
  && echo "PASS: action.yml passes the step instance id into the suffix" \
  || { echo "FAIL: action.yml does not pass the step instance id into the suffix"; fail=1; }

[ "$fail" -eq 0 ] || exit 1
echo "All artifact name uniqueness checks passed."
