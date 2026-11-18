#!/usr/bin/env bash
# Structural checks for the rolling extension CI cache steps in action.yml.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ACTION_FILE="${ROOT_DIR}/action.yml"

fail() { echo "FAIL: $*" >&2; exit 1; }

step_block() {
  awk -v name="    - name: $1" '
    $0 == name { in_block = 1; print; next }
    in_block && /^    - name: / { exit }
    in_block { print }
  ' "${ACTION_FILE}"
}

step_line() { grep -n "^    - name: $1\$" "${ACTION_FILE}" | cut -d: -f1; }

legacy="$(step_block 'Restore extension CI cache')"
grep -q "steps.extension-cache.outputs.rolling != 'true'" <<<"${legacy}" \
  || fail 'lockfile-keyed restore must not handle rolling caches'
grep -q 'uses: actions/cache@v5' <<<"${legacy}" || fail 'lockfile-keyed cache keeps restore+post-save'

restore="$(step_block 'Restore rolling extension CI cache')"
[ -n "${restore}" ] || fail 'missing rolling restore step'
# restore-only: a post-job save would freeze the cache on its first key.
grep -q 'uses: actions/cache/restore@v5' <<<"${restore}" || fail 'rolling restore must be restore-only'
grep -q "steps.extension-cache.outputs.populated != 'true'" <<<"${restore}" \
  || fail 'rolling restore must be skipped when the store is already populated'
grep -q 'restore-keys: ${{ runner.os }}-${{ steps.extension-cache.outputs.namespace }}-rolling-$' <<<"${restore}" \
  || fail 'rolling restore must take the newest entry under the namespace prefix'

snapshot="$(step_block 'Snapshot rolling extension CI cache')"
grep -q 'id: rolling-cache-snapshot' <<<"${snapshot}" || fail 'snapshot step id changed'
grep -q 'extension-cache-digest.sh snapshot' <<<"${snapshot}" || fail 'snapshot must record the restored digest'

plan="$(step_block 'Plan rolling extension CI cache save')"
grep -q "if: \${{ !cancelled() && steps.rolling-cache-snapshot.outcome == 'success' }}" <<<"${plan}" \
  || fail 'save plan must run after failed and dry-run commands too'
grep -q 'RESTORED_DIGEST: ${{ steps.rolling-cache-snapshot.outputs.digest }}' <<<"${plan}" \
  || fail 'save plan must compare against the restored digest'

save="$(step_block 'Save rolling extension CI cache')"
grep -q 'uses: actions/cache/save@v5' <<<"${save}" || fail 'rolling save must use actions/cache/save'
grep -q 'continue-on-error: true' <<<"${save}" || fail 'a cache save failure must not fail the run'
grep -q "steps.rolling-cache-plan.outputs.save == 'true'" <<<"${save}" || fail 'save only when contents changed'
grep -q 'key: ${{ runner.os }}-${{ steps.extension-cache.outputs.namespace }}-rolling-${{ steps.rolling-cache-plan.outputs.digest }}' <<<"${save}" \
  || fail 'rolling save must be keyed by content digest'
grep -q 'path: ${{ steps.extension-cache.outputs.paths }}' <<<"${save}" \
  || fail 'save paths must match restore paths (cache version)'

# Order: restore -> snapshot -> every command step -> plan -> save -> enforcement.
restore_line="$(step_line 'Restore rolling extension CI cache')"
snapshot_line="$(step_line 'Snapshot rolling extension CI cache')"
plan_line="$(step_line 'Plan rolling extension CI cache save')"
save_line="$(step_line 'Save rolling extension CI cache')"
for command_step in 'Run Homeboy commands' 'Run release' 'Run operations (deploy/git)'; do
  line="$(grep -nF "    - name: ${command_step}" "${ACTION_FILE}" | head -1 | cut -d: -f1)"
  [ -n "${line}" ] || fail "missing command step '${command_step}'"
  [ "${snapshot_line}" -lt "${line}" ] || fail "snapshot must precede '${command_step}'"
  [ "${plan_line}" -gt "${line}" ] || fail "save plan must follow '${command_step}'"
done
enforce_line="$(step_line 'Enforce final status')"
[ "${restore_line}" -lt "${snapshot_line}" ] || fail 'snapshot must follow the restore'
[ "${plan_line}" -lt "${save_line}" ] && [ "${save_line}" -lt "${enforce_line}" ] \
  || fail 'save must run before final status enforcement can fail the job'

echo 'Rolling extension cache wiring checks passed'
