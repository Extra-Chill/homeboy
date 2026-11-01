#!/usr/bin/env bash

# Every permission the reusable release workflow requests is forced on every
# caller.
#
# A called reusable workflow cannot hold more permissions than its caller
# grants. So each scope listed in release.yml's top-level `permissions` is a
# scope every consumer must also list. Adding one is a breaking change that no
# consumer can absorb by doing nothing — GitHub rejects the run before any job
# exists, and reports it as a bare `startup_failure` with no job, no log and no
# annotation.
#
# That happened in v2.20.0 (#494): `id-token: write` was added for npm trusted
# publishing, and because `v2` is a floating tag it instantly broke roughly 30
# WordPress repositories that will never publish an npm package. Diagnosis took
# far longer than the fix, because nothing in a consumer repository looks wrong
# — the workflow is valid, active, and unchanged.
#
# This guard pins the required set. Changing it should be a deliberate act with
# a major-version release, not an incidental line in a feature PR.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKFLOW="${ROOT_DIR}/.github/workflows/release.yml"
MINIMAL="${ROOT_DIR}/fixtures/reusable-release-minimal-consumer.yml"

fail=0
pass() { echo "PASS: $1"; }
bad() { echo "FAIL: $1"; fail=1; }

# release.yml must not declare workflow-level permissions. Requesting a scope
# forces it on every caller (#494); listing any scope also downgrades every
# unlisted one, which silently dropped callers' id-token: write and broke npm
# trusted publishing (ENEEDAUTH). Jobs inherit the caller's grants instead.
if grep -qE '^permissions:' "${WORKFLOW}"; then
  bad "release.yml declares workflow-level permissions (forces scopes on callers and drops id-token)"
else
  pass "release.yml inherits caller permissions"
fi

# No job may request id-token: it belongs only to npm-publishing callers.
if grep -qE '^ +id-token:' "${WORKFLOW}"; then
  bad "release.yml requests id-token, forcing it on every caller (#494)"
else
  pass "release.yml does not force id-token on callers"
fi

# Job-level permissions also downgrade unlisted scopes, so the jobs that run
# release/publish must not declare them.
for job in monorepo release; do
  if awk -v job="${job}" '
    $0 ~ "^  " job ":$" { injob = 1; next }
    injob && /^  [a-z-]+:$/ { exit }
    injob && /^    permissions:/ { found = 1; exit }
    END { exit found ? 0 : 1 }
  ' "${WORKFLOW}"; then
    bad "job ${job} declares permissions, dropping the caller's id-token"
  else
    pass "job ${job} inherits caller permissions"
  fi
done

expected="$(printf '%s\n' contents issues pull-requests | sort)"

# The minimal consumer documents the contract, so it must match it exactly.
minimal_perms="$(awk '
  /^permissions:/ { inblock = 1; next }
  inblock && /^[^ #]/ { exit }
  inblock && /^  [a-z-]+: / { sub(/:.*/, "", $1); print $1 }
' "${MINIMAL}" | sort)"

if [ "${minimal_perms}" = "${expected}" ]; then
  pass "the minimal consumer fixture grants exactly the required scopes"
else
  bad "the minimal consumer fixture drifted from the required scopes"
  echo "  expected: $(echo "${expected}" | tr '\n' ' ')"
  echo "  found:    $(echo "${minimal_perms}" | tr '\n' ' ')"
fi

# A consumer that does publish to npm needs somewhere to look.
NPM_FIXTURE="${ROOT_DIR}/fixtures/reusable-release-npm-consumer.yml"
if [ -f "${NPM_FIXTURE}" ] && grep -qE '^  id-token: write' "${NPM_FIXTURE}"; then
  pass "an npm-publishing consumer fixture documents the extra scope"
else
  bad "no npm consumer fixture showing where id-token belongs"
fi

if [ "${fail}" -ne 0 ]; then
  echo "reusable release caller permission checks FAILED"
  exit 1
fi
echo "All reusable release caller permission checks passed."
