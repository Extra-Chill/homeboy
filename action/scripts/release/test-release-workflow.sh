#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
REUSABLE_RELEASE="${ROOT_DIR}/.github/workflows/release.yml"
PREPARED_REF_TEST="${ROOT_DIR}/scripts/release/test-resolve-prepared-ref.sh"
RELEASE_CONSUMER_FIXTURE="${ROOT_DIR}/fixtures/reusable-release-minimal-consumer.yml"
CI_WORKFLOW="${ROOT_DIR}/.github/workflows/ci.yml"
SELF_TEST_WORKFLOW="${ROOT_DIR}/.github/workflows/self-test.yml"
RUN_RELEASE="${ROOT_DIR}/scripts/release/run-release.sh"
README="${ROOT_DIR}/README.md"
COMMENT_SECTIONS="${ROOT_DIR}/scripts/pr/comment/sections.sh"
VERSION_FILE="${ROOT_DIR}/VERSION"
HOMEBOY_CONFIG="${ROOT_DIR}/homeboy.json"

assert_not_contains() {
  local needle="$1"
  local file_path="$2"
  local label="$3"

  if grep -q -- "${needle}" "${file_path}"; then
    printf 'FAIL: %s\nfound forbidden pattern: %s\nfile: %s\n' "${label}" "${needle}" "${file_path}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_contains() {
  local needle="$1"
  local file_path="$2"
  local label="$3"

  if ! grep -q -- "${needle}" "${file_path}"; then
    printf 'FAIL: %s\nmissing pattern: %s\nfile: %s\n' "${label}" "${needle}" "${file_path}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_count() {
  local needle="$1"
  local expected="$2"
  local file_path="$3"
  local label="$4"
  local actual

  actual="$(grep -c -- "${needle}" "${file_path}" || true)"
  if [ "${actual}" != "${expected}" ]; then
    printf 'FAIL: %s\nexpected %s occurrence(s) of: %s\nactual: %s\nfile: %s\n' "${label}" "${expected}" "${needle}" "${actual}" "${file_path}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}


# GitHub-hosted workflow dependencies must use Node 24-capable action majors (#321).
assert_count 'actions/checkout@v6' '21' "${CI_WORKFLOW}" "reusable CI workflow uses checkout v6 for every checkout"
assert_count 'actions/cache/restore@v5' '1' "${CI_WORKFLOW}" "reusable CI workflow restores one candidate build cache"
assert_count 'actions/cache/save@v5' '1' "${CI_WORKFLOW}" "reusable CI workflow saves one shared candidate build cache"
assert_count 'actions/cache@v5' '2' "${ROOT_DIR}/action.yml" "composite action owns release and extension caches"
assert_count 'actions/cache/restore@v5' '1' "${ROOT_DIR}/action.yml" "composite action restores source-build cache"
assert_count 'actions/cache/save@v5' '1' "${ROOT_DIR}/action.yml" "composite action saves shared source-build cache"
assert_contains 'actions/upload-artifact@v7' "${CI_WORKFLOW}" "reusable CI workflow uses artifact upload v7"
assert_not_contains 'actions/upload-artifact@[^v]\|actions/upload-artifact@v[^7]\|actions/upload-artifact@v7[[:alnum:]._-]' "${CI_WORKFLOW}" "every reusable CI upload uses the supported v7 contract"
assert_count 'actions/download-artifact@v7' '8' "${CI_WORKFLOW}" "reusable CI workflow uses artifact download v7"
assert_count 'actions/create-github-app-token@v3' '4' "${CI_WORKFLOW}" "reusable CI workflow uses app-token v3"
assert_not_contains 'actions/checkout@v4' "${CI_WORKFLOW}" "reusable CI workflow has no checkout v4 references"
assert_not_contains 'actions/cache@v4' "${CI_WORKFLOW}" "reusable CI workflow has no cache v4 references"
assert_not_contains 'actions/\(upload\|download\)-artifact@v4' "${CI_WORKFLOW}" "reusable CI workflow has no artifact v4 references"
assert_not_contains 'actions/create-github-app-token@v2' "${CI_WORKFLOW}" "reusable CI workflow has no app-token v2 references"
assert_not_contains 'merge-multiple: true' "${CI_WORKFLOW}" "reusable CI workflow never merges competing binary artifacts"
assert_contains 'actions: read' "${CI_WORKFLOW}" "reusable CI workflow can download current-run artifacts"
assert_contains 'actions/checkout@v6' "${SELF_TEST_WORKFLOW}" "self-test workflow uses checkout v6"
assert_not_contains 'actions/checkout@v4' "${SELF_TEST_WORKFLOW}" "self-test workflow has no checkout v4 references"
assert_count 'actions/upload-artifact@v7' '4' "${ROOT_DIR}/action.yml" "composite action uses artifact upload v7"
assert_count 'actions/download-artifact@v7' '1' "${ROOT_DIR}/action.yml" "composite action uses artifact download v7"
assert_not_contains 'actions/\(upload\|download\)-artifact@v4' "${ROOT_DIR}/action.yml" "composite action has no artifact v4 references"

# Releasing moves the floating v2 tag, so the action's own shell tests must
# gate publication rather than running advisory-only (or not at all). Releases
# are cut from Extra-Chill/homeboy (action/); its action-release.yml is only
# present in that checkout, not in the published copy.
MONOREPO_RELEASE="${ROOT_DIR}/../.github/workflows/action-release.yml"
if [ -f "${MONOREPO_RELEASE}" ]; then
  assert_contains 'uses: ./.github/workflows/action-ci.yml' "${MONOREPO_RELEASE}" "release is gated on the action self-test suite"
  assert_contains 'needs: tests' "${MONOREPO_RELEASE}" "release job waits for the self-tests"
  assert_contains 'workflow consumer-contract.yml' "${MONOREPO_RELEASE}" "publication waits for the GitHub consumer contract on the published tag"
  assert_contains 'Move the floating major tag' "${MONOREPO_RELEASE}" "the major tag moves only in the publish job"
  assert_not_contains 'post:release' "${HOMEBOY_CONFIG}" "release does not move v2 from a post-release hook"
fi
assert_contains 'bash scripts/run-tests.sh' "${SELF_TEST_WORKFLOW}" "self-test workflow runs every action shell test"
assert_contains 'pull_request' "${SELF_TEST_WORKFLOW}" "self-test workflow also guards pull requests"

assert_not_contains '^  --skip-publish$' "${RUN_RELEASE}" "run-release does not hardcode skipped publish steps"
assert_not_contains '^  --no-github-release$' "${RUN_RELEASE}" "run-release does not hardcode skipped GitHub Release creation"
assert_contains 'RELEASE_SKIP_PUBLISH' "${RUN_RELEASE}" "run-release exposes publish opt-out env"
assert_contains 'RELEASE_SKIP_GITHUB_RELEASE' "${RUN_RELEASE}" "run-release exposes GitHub Release opt-out env"
assert_contains '--git-identity bot' "${RUN_RELEASE}" "run-release delegates bot identity to Homeboy core"
assert_contains 'GIT_ASKPASS' "${RUN_RELEASE}" "run-release provides token auth to Homeboy git pushes"
assert_not_contains 'git config user.name' "${RUN_RELEASE}" "run-release does not configure git identity directly"
assert_not_contains 'git config --local' "${RUN_RELEASE}" "run-release does not configure git auth directly"
assert_not_contains 'remote set-url origin' "${RUN_RELEASE}" "run-release does not rewrite origin to an authenticated URL"
assert_contains 'homeboy_verify_github_release_published' "${RUN_RELEASE}" "run-release asserts the release actually published before reporting success"
assert_contains 'release-verify-github-release:' "${ROOT_DIR}/action.yml" "action exposes GitHub Release verification toggle"
assert_contains 'HOMEBOY_VERIFY_GITHUB_RELEASE: ${{ inputs.release-verify-github-release }}' "${ROOT_DIR}/action.yml" "action passes verification toggle to release script"
assert_contains 'run-release-with-liveness.sh' "${ROOT_DIR}/action.yml" "action routes releases through the shared liveness boundary"
assert_contains 'HOMEBOY_ACTION_PHASE_HEARTBEAT_SECONDS: ${{ inputs.phase-heartbeat-seconds }}' "${ROOT_DIR}/action.yml" "action passes heartbeat settings to release phases"
assert_contains 'HOMEBOY_ACTION_PHASE_BUDGET_SECONDS: ${{ inputs.phase-budget-seconds }}' "${ROOT_DIR}/action.yml" "action passes phase budgets to release phases"
assert_contains 'id: artifact-names' "${ROOT_DIR}/action.yml" "action computes matrix-safe artifact names"
assert_contains 'PORTABLE_ID_VALUE: ${{ steps.read-config.outputs.portable-id }}' "${ROOT_DIR}/action.yml" "artifact names include component identity"
assert_contains 'PHP_VALUE: ${{ steps.detect-env.outputs.portable-php || steps.provisioning.outputs.portable-php }}' "${ROOT_DIR}/action.yml" "artifact names include PHP matrix identity"
assert_contains 'NODE_VALUE: ${{ steps.detect-env.outputs.portable-node || steps.provisioning.outputs.portable-node }}' "${ROOT_DIR}/action.yml" "artifact names include Node matrix identity"
assert_contains 'name: ${{ steps.artifact-names.outputs.homeboy-ci-results }}' "${ROOT_DIR}/action.yml" "CI result artifacts use computed names"
assert_contains 'name: ${{ steps.artifact-names.outputs.homeboy-observations }}' "${ROOT_DIR}/action.yml" "observation artifacts use computed names"
assert_contains 'import-observations:' "${ROOT_DIR}/action.yml" "action exposes opt-in observation import"
assert_contains 'pattern: homeboy-observations-*' "${ROOT_DIR}/action.yml" "action downloads observation artifacts by pattern"
assert_contains 'scripts/core/import-observations.sh' "${ROOT_DIR}/action.yml" "action imports downloaded observation artifacts"
assert_contains 'HOMEBOY_OBSERVATIONS_ARTIFACT' "${ROOT_DIR}/action.yml" "PR comment receives observation artifact name"
assert_not_contains 'name: homeboy-ci-results$' "${ROOT_DIR}/action.yml" "CI result artifacts do not use a shared static name"
assert_not_contains 'name: homeboy-observations$' "${ROOT_DIR}/action.yml" "observation artifacts do not use a shared static name"
assert_contains 'release-bump-type:' "${ROOT_DIR}/action.yml" "action exposes release bump type output"
assert_contains 'skipped-reason:' "${ROOT_DIR}/action.yml" "action exposes skipped release reason output"

# Failed-SHA marker escape hatches (homeboy-action#257):
# 1. A manual dispatch must never be blocked by a stale marker.
# 2. The marker is keyed by tooling identity so a CI-side fix self-heals.
assert_contains 'tooling-identity:' "${ROOT_DIR}/action.yml" "action exposes resolved tooling identity output"
assert_contains '^2\.' "${VERSION_FILE}" "VERSION is aligned with the v2 action channel"
assert_not_contains 'Extra-Chill/homeboy-action@v1' "${README}" "README examples use the v2 action channel"
assert_contains 'Extra-Chill/homeboy-action@v2' "${README}" "README documents the v2 action channel"
assert_not_contains 'Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v1' "${COMMENT_SECTIONS}" "PR comment footer does not advertise v1"
assert_not_contains 'Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v2' "${COMMENT_SECTIONS}" "PR comment footer does not duplicate action metadata"

# Reusable consumer release workflow (#472). It runs in the CALLER's context,
# where `./` would resolve to the caller checkout, so it must invoke the
# published action by ref — resolved once to an immutable revision in the
# check job and reused by the release job, so the dry-run that computed
# tooling-identity and the release gated on it share one tooling revision.
assert_contains 'workflow_call:' "${REUSABLE_RELEASE}" "reusable release workflow is callable"
assert_contains 'action-ref:' "${REUSABLE_RELEASE}" "reusable release workflow exposes action-ref revision pinning"
# `uses:` does not accept expressions, so the resolved revision is checked out
# into .homeboy-action and invoked by local path — the reusable CI contract.
assert_contains 'ref: ${{ steps.action-sha.outputs.sha }}' "${REUSABLE_RELEASE}" "reusable release check checks out the resolved action revision"
assert_contains 'ref: ${{ needs.check.outputs.action-sha }}' "${REUSABLE_RELEASE}" "reusable release job checks out the same resolved action revision as the check"
assert_contains 'uses: ./.homeboy-action' "${REUSABLE_RELEASE}" "reusable release workflow invokes the nested action checkout by local path"
assert_not_contains 'uses: Extra-Chill/homeboy-action@${{' "${REUSABLE_RELEASE}" "reusable release workflow never interpolates an expression into uses:"
assert_not_contains 'uses: \./$' "${REUSABLE_RELEASE}" "reusable release workflow never uses the bare caller-relative ./ path"
assert_contains ".homeboy-action/' >> .git/info/exclude" "${REUSABLE_RELEASE}" "reusable release workflow excludes the nested action checkout from the release working-tree gate"
assert_contains 'startsWith(github.event.head_commit.message' "${REUSABLE_RELEASE}" "reusable release workflow skips pushes of release commits"
assert_contains "github.event_name == 'workflow_dispatch'" "${REUSABLE_RELEASE}" "reusable release workflow detects manual dispatch for the marker bypass"
assert_contains 'IS_MANUAL_DISPATCH' "${REUSABLE_RELEASE}" "reusable release workflow gates the failed-SHA skip on non-dispatch runs"
assert_contains 'release-last-failed-${{ github.ref_name }}-${{ steps.source-sha.outputs.source-sha }}-${{ inputs.component || '\''root'\'' }}-${{ steps.release-check.outputs.tooling-identity }}' "${REUSABLE_RELEASE}" "reusable release failure-cache restore key includes source, component, and tooling identity"
assert_contains 'release-last-failed-${{ github.ref_name }}-${{ needs.check.outputs.source-sha }}-${{ inputs.component || '\''root'\'' }}-${{ needs.check.outputs.tooling-identity }}' "${REUSABLE_RELEASE}" "reusable release failure-cache save key includes source, component, and tooling identity"
assert_not_contains 'restore-keys:' "${REUSABLE_RELEASE}" "reusable release failure cache never falls back to a stale-tooling marker via restore-keys"
assert_contains 'persist-credentials: false' "${REUSABLE_RELEASE}" "reusable release checkout does not persist credentials"
assert_contains 'value: ${{ jobs.release.outputs.release-version }}' "${REUSABLE_RELEASE}" "reusable release workflow re-exports the action release-version output"
assert_contains 'value: ${{ jobs.check.outputs.tooling-identity }}' "${REUSABLE_RELEASE}" "reusable release workflow re-exports tooling-identity"
assert_contains 'value: ${{ jobs.check.outputs.source-sha }}' "${REUSABLE_RELEASE}" "reusable release workflow exposes resolved source SHA"
assert_contains 'value: ${{ jobs.release.outputs.released-source-sha }}' "${REUSABLE_RELEASE}" "reusable release workflow exposes released source SHA"
assert_contains '      component:' "${REUSABLE_RELEASE}" "reusable release workflow exposes component selection"
assert_contains '      prepared-ref:' "${REUSABLE_RELEASE}" "reusable release workflow exposes prepared source refs"
assert_contains '      publisher-known-hosts:' "${REUSABLE_RELEASE}" "reusable release workflow exposes pinned publisher host input"
assert_contains '      PUBLISH_SSH_KEY:' "${REUSABLE_RELEASE}" "reusable release workflow declares optional publisher key secret"
assert_count 'component: ${{ inputs.component }}' '2' "${REUSABLE_RELEASE}" "component input reaches dry-run and release paths"
assert_contains 'resolve-prepared-ref.sh' "${REUSABLE_RELEASE}" "prepared source refs use the canonical resolver"
assert_contains 'source-sha: ${{ steps.source-sha.outputs.source-sha }}' "${REUSABLE_RELEASE}" "check job exports the resolved source SHA"
assert_contains 'ref: ${{ needs.check.outputs.source-sha }}' "${REUSABLE_RELEASE}" "release job checks out the checked source SHA"
assert_contains 'released-source-sha: ${{ steps.revalidate-source.outputs.source-sha }}' "${REUSABLE_RELEASE}" "release job exports its revalidated source SHA"
assert_contains 'id: revalidate-source' "${REUSABLE_RELEASE}" "release job revalidates the prepared source before release"
assert_contains 'SOURCE_TOKEN:' "${REUSABLE_RELEASE}" "prepared source resolution supports private repositories"
assert_contains 'ssh-key: ${{ secrets.PUBLISH_SSH_KEY }}' "${REUSABLE_RELEASE}" "publisher key reaches only the real release action"
assert_contains "ssh-require-known-hosts: 'true'" "${REUSABLE_RELEASE}" "publisher release requires strict host verification"
assert_contains 'ssh-require-known-hosts:' "${ROOT_DIR}/action.yml" "composite action exposes strict SSH host verification"
assert_contains 'cleanup-ssh.sh' "${ROOT_DIR}/action.yml" "composite action cleans publisher credentials"
assert_not_contains 'SSH_AGENT_PID: ${{ env.SSH_AGENT_PID }}' "${ROOT_DIR}/action.yml" "SSH cleanup reads runtime agent state from GITHUB_ENV"
assert_contains "inputs.component || 'root'" "${REUSABLE_RELEASE}" "failure cache preserves root compatibility while isolating components"
assert_contains 'CACHE_PREFIX:' "${REUSABLE_RELEASE}" "failure cache cleanup receives a separate safe prefix value"
assert_contains 'jq -r --arg prefix "${CACHE_PREFIX}"' "${REUSABLE_RELEASE}" "failure cache cleanup safely handles component paths"
DISPATCH_SCRIPT="${ROOT_DIR}/scripts/release/dispatch-releases.sh"
assert_contains "ref: \${{ inputs.components == '' && needs.release.outputs.release-tag || '' }}" "${REUSABLE_RELEASE}" "dispatch checks out the single-mode released tag with repository credentials"
assert_contains 'git rev-parse --verify --quiet "${tag}^{commit}"' "${DISPATCH_SCRIPT}" "dispatch resolves lightweight and annotated tags to the released commit"
assert_not_contains 'git ls-remote "https://github.com/${GITHUB_REPOSITORY}.git"' "${REUSABLE_RELEASE}" "dispatch does not resolve private tags anonymously"
assert_contains 'prepared-ref' "${ROOT_DIR}/scripts/release/test-resolve-prepared-ref.sh" "prepared-ref behavior has a focused test"

python3 - "${REUSABLE_RELEASE}" <<'PY'
import sys

workflow = open(sys.argv[1]).read()
monorepo, workflow = workflow.split('  # ── Step 1:', 1)
assert "if: inputs.components != ''" in monorepo
assert "inputs.components == ''" in workflow
assert '      components:' in monorepo
assert 'value: ${{ jobs.monorepo.outputs.results }}' in monorepo
assert monorepo.count('resolve-prepared-ref.sh') == 1
assert 'PREPARED_REF: ${{ inputs.prepared-ref || github.sha }}' in monorepo
assert 'uses: ./.homeboy-monorepo' in monorepo
assert 'run-monorepo-releases.py generate' in monorepo
assert 'homeboy-monorepo-releases.json' in monorepo
assert 'needs: check' not in monorepo
print('PASS: monorepo mode is isolated, serial, aggregated, and validates source once')
action_checkout = workflow.index("path: .homeboy-action")
resolver = workflow.index("run: bash .homeboy-action/scripts/release/resolve-prepared-ref.sh")
source_checkout = workflow.index("ref: ${{ steps.source-sha.outputs.source-sha }}")
if not action_checkout < resolver < source_checkout:
    raise SystemExit("prepared resolver/action checkout ordering is unsafe")
if "clean: false" not in workflow[source_checkout:workflow.index("# The nested action checkout", source_checkout)]:
    raise SystemExit("consumer source checkout could remove the resolver checkout")
print("PASS: prepared resolver remains available across consumer checkout ordering")
PY

# The canonical minimal release caller is release-gated with the workflow it
# documents: this script runs in self-test, which gates every release.
assert_contains 'uses: Extra-Chill/homeboy-action/.github/workflows/release.yml@v2' "${RELEASE_CONSUMER_FIXTURE}" "minimal release consumer fixture calls the published reusable release workflow"
assert_contains 'secrets: inherit' "${RELEASE_CONSUMER_FIXTURE}" "minimal release consumer fixture inherits secrets"
assert_contains 'dry-run: true' "${RELEASE_CONSUMER_FIXTURE}" "minimal release consumer fixture defaults to dry-run"
assert_contains '  contents: write' "${RELEASE_CONSUMER_FIXTURE}" "minimal release consumer fixture grants contents write"
assert_contains '  pull-requests: write' "${RELEASE_CONSUMER_FIXTURE}" "minimal release consumer fixture grants pull-requests write"
assert_contains '  issues: write' "${RELEASE_CONSUMER_FIXTURE}" "minimal release consumer fixture grants issues write"

# Post-release dispatch (#476): declared, gated on a real successful release,
# never fires on dry-run, and never relies on GITHUB_TOKEN for cross-repo.
DISPATCH_CONSUMER_FIXTURE="${ROOT_DIR}/fixtures/reusable-release-dispatch-consumer.yml"
assert_contains '      dispatch-repo:' "${REUSABLE_RELEASE}" "reusable release workflow exposes dispatch-repo"
assert_contains '      dispatch-event:' "${REUSABLE_RELEASE}" "reusable release workflow exposes dispatch-event"
assert_contains '      DISPATCH_TOKEN:' "${REUSABLE_RELEASE}" "reusable release workflow accepts an optional DISPATCH_TOKEN secret"
assert_contains '  dispatch:' "${REUSABLE_RELEASE}" "reusable release workflow declares the dispatch job"
assert_contains "inputs.dispatch-repo != '' && inputs.dry-run != true" "${REUSABLE_RELEASE}" "dispatch job requires a target and no dry-run"
assert_contains "needs.check.outputs.should-release == 'true' && needs.release.outputs.released == 'true'" "${REUSABLE_RELEASE}" "single-mode dispatch is gated on a real successful release"
assert_contains '"repos/${DISPATCH_REPO}/dispatches"' "${DISPATCH_SCRIPT}" "dispatch script posts a repository_dispatch to the target"
assert_contains 'GITHUB_TOKEN cannot send repository_dispatch cross-repo' "${DISPATCH_SCRIPT}" "dispatch script fails closed without a cross-repo-capable token"
assert_contains 'contents:write on the target' "${DISPATCH_SCRIPT}" "dispatch documentation uses contents write permission"
assert_contains 'value: ${{ jobs.dispatch.result == '"'"'success'"'"' }}' "${REUSABLE_RELEASE}" "reusable release workflow re-exports dispatched"
assert_contains 'component: ${{ steps.release.outputs.component }}' "${REUSABLE_RELEASE}" "release job exposes the portable component id for the payload"
assert_contains 'dispatch-repo: example-org/example-deploy' "${DISPATCH_CONSUMER_FIXTURE}" "dispatch consumer fixture sets a dispatch target"
assert_contains 'dry-run: true' "${DISPATCH_CONSUMER_FIXTURE}" "dispatch consumer fixture stays dry-run so it can never dispatch"
assert_contains 'secrets: inherit' "${DISPATCH_CONSUMER_FIXTURE}" "dispatch consumer fixture inherits secrets"

# Monorepo dispatch (Extra-Chill/homeboy#17345): `components: all` must not
# silently ignore dispatch-repo. Both modes share one dispatch job and script.
python3 - "${REUSABLE_RELEASE}" <<'PY'
import sys

workflow = open(sys.argv[1]).read()
monorepo = workflow.split('  # ── Step 1:', 1)[0].split('  monorepo:', 1)[1]
dispatch = workflow.split('  dispatch:\n', 1)[1].split('  # ── Record failed SHA', 1)[0]
assert 'released: ${{ steps.released.outputs.released }}' in monorepo
assert 'released-count: ${{ steps.released.outputs.released-count }}' in monorepo
collect = monorepo.split('id: released', 1)[1]
assert "if: always() && steps.source.outputs.stale != 'true'" in collect, 'collect runs even when a component failed'
assert 'DRY_RUN: ${{ inputs.dry-run }}' in collect
assert 'run-monorepo-releases.py released' in collect
needs = dispatch.split('    if:', 1)[0]
for job in ('monorepo', 'check', 'release'):
    assert f'      - {job}\n' in needs, job
gate = dispatch.split('    if:', 1)[1].split('    runs-on:', 1)[0]
assert '!cancelled()' in gate
assert "inputs.dispatch-repo != ''" in gate and 'inputs.dry-run != true' in gate
assert "inputs.components != '' && needs.monorepo.outputs.released-count != ''" in gate
assert "needs.monorepo.outputs.released-count != '0'" in gate
assert "inputs.components == '' && needs.release.result == 'success'" in gate
assert "RELEASES_JSON: ${{ inputs.components != '' && needs.monorepo.outputs.released || '' }}" in dispatch
assert 'DISPATCH_REPO: ${{ inputs.dispatch-repo }}' in dispatch
assert 'DISPATCH_TOKEN: ${{ secrets.DISPATCH_TOKEN }}' in dispatch
assert 'repositories: ${{ steps.target.outputs.repo }}' in dispatch
assert dispatch.count('run: bash .homeboy-action/scripts/release/dispatch-releases.sh') == 1
assert dispatch.index('fetch-depth: 0') < dispatch.index('path: .homeboy-action'), 'action checkout must follow the consumer checkout'
assert '"repos/' not in dispatch, 'dispatch POST lives only in the shared script'
print('PASS: monorepo mode wires dispatch-repo into the shared dispatch step')
PY

# Behavior of the shared dispatch script against a fake `gh`.
DISPATCH_TMP="$(mktemp -d)"
trap 'rm -rf "${DISPATCH_TMP}"' EXIT
mkdir -p "${DISPATCH_TMP}/bin" "${DISPATCH_TMP}/calls"
cat > "${DISPATCH_TMP}/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
n="$(find "${GH_CALLS}" -name 'call-*.json' | wc -l | tr -d ' ')"
input=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --input) input="$2"; shift 2 ;;
    *) printf '%s\n' "$1" >> "${GH_CALLS}/args-${n}"; shift ;;
  esac
done
cp "${input}" "${GH_CALLS}/call-${n}.json"
printf '%s\n' "${GH_TOKEN}" > "${GH_CALLS}/token-${n}"
if [ -n "${GH_FAIL_COMPONENT:-}" ] && jq -e --arg c "${GH_FAIL_COMPONENT}" '.client_payload.component == $c' "${input}" >/dev/null; then
  exit 1
fi
GH
chmod +x "${DISPATCH_TMP}/bin/gh"
dispatch_repo_dir="${DISPATCH_TMP}/repo"
git init -q "${dispatch_repo_dir}"
git -C "${dispatch_repo_dir}" -c user.email=t@example.test -c user.name=T commit -q --allow-empty -m one
git -C "${dispatch_repo_dir}" tag alpha-v1.0.0
git -C "${dispatch_repo_dir}" -c user.email=t@example.test -c user.name=T commit -q --allow-empty -m two
git -C "${dispatch_repo_dir}" -c user.email=t@example.test -c user.name=T tag -a beta-v2.1.0 -m beta
git -C "${dispatch_repo_dir}" tag v3.0.0
alpha_sha="$(git -C "${dispatch_repo_dir}" rev-parse alpha-v1.0.0)"
beta_sha="$(git -C "${dispatch_repo_dir}" rev-parse 'beta-v2.1.0^{commit}')"
run_url='https://github.com/example-org/example-plugins/actions/runs/1'

run_dispatch() {
  rm -f "${DISPATCH_TMP}"/calls/*
  (cd "${dispatch_repo_dir}" && env PATH="${DISPATCH_TMP}/bin:${PATH}" GH_CALLS="${DISPATCH_TMP}/calls" \
    RUNNER_TEMP="${DISPATCH_TMP}" GITHUB_REPOSITORY=example-org/example-plugins \
    DISPATCH_REPO=example-org/example-deploy DISPATCH_EVENT=component-released \
    RUN_URL="${run_url}" DISPATCH_TOKEN= APP_TOKEN= RELEASES_JSON= \
    "$@" bash "${DISPATCH_SCRIPT}") > "${DISPATCH_TMP}/out" 2>&1
}
calls() { find "${DISPATCH_TMP}/calls" -name 'call-*.json' | wc -l | tr -d ' '; }
expect() {
  if [ "$1" != "$2" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual: %s\n' "$3" "$2" "$1"
    cat "${DISPATCH_TMP}/out"
    exit 1
  fi
  printf 'PASS: %s\n' "$3"
}

two_released='[{"component":"alpha","version":"1.0.0","tag":"alpha-v1.0.0","bump":"minor"},{"component":"beta","version":"2.1.0","tag":"beta-v2.1.0","bump":"patch"}]'
run_dispatch DISPATCH_TOKEN=pat APP_TOKEN=app RELEASES_JSON="${two_released}"
expect "$(calls)" 2 "monorepo dispatch sends one event per released component"
expect "$(jq -c . "${DISPATCH_TMP}/calls/call-0.json")" \
  "$(jq -nc --arg sha "${alpha_sha}" --arg run_url "${run_url}" '{event_type:"component-released",client_payload:{component:"alpha",repository:"example-org/example-plugins",version:"1.0.0",tag:"alpha-v1.0.0",bump:"minor",sha:$sha,run_url:$run_url}}')" \
  "monorepo payload matches the single-component payload shape"
expect "$(jq -r .client_payload.sha "${DISPATCH_TMP}/calls/call-1.json")" "${beta_sha}" "annotated monorepo tag dispatches its peeled commit"
expect "$(cat "${DISPATCH_TMP}/calls/token-0")" pat "DISPATCH_TOKEN wins over the app token"
expect "$(cat "${DISPATCH_TMP}"/calls/args-* | grep -c '^repos/example-org/example-deploy/dispatches$')" 2 "every event posts to the dispatch target"

run_dispatch APP_TOKEN=app COMPONENT=root-plugin VERSION=3.0.0 TAG=v3.0.0 BUMP=major
expect "$(calls)" 1 "single-component dispatch sends exactly one event"
expect "$(jq -r '.client_payload | [.component,.version,.tag,.bump] | join(" ")' "${DISPATCH_TMP}/calls/call-0.json")" "root-plugin 3.0.0 v3.0.0 major" "single-component payload comes from the release outputs"
expect "$(cat "${DISPATCH_TMP}/calls/token-0")" app "scoped app token is the fallback"

run_dispatch DISPATCH_TOKEN=pat RELEASES_JSON='[]'
expect "$(calls)" 0 "nothing is dispatched when nothing was released"

status=0
run_dispatch RELEASES_JSON="${two_released}" || status=$?
expect "${status}:$(calls)" "1:0" "dispatch fails closed without a cross-repo token"

status=0
run_dispatch DISPATCH_TOKEN=pat GH_FAIL_COMPONENT=alpha \
  RELEASES_JSON='[{"component":"alpha","version":"1.0.0","tag":"alpha-v1.0.0","bump":"minor"},{"component":"gone","version":"9.0.0","tag":"gone-v9.0.0","bump":"major"},{"component":"beta","version":"2.1.0","tag":"beta-v2.1.0","bump":"patch"}]' || status=$?
expect "${status}:$(calls)" "1:2" "a failed or unresolvable dispatch does not stop the remaining components"

# The released list feeding dispatch: dry runs and non-releases send nothing.
released_count() {
  printf '%s' "$1" > "${DISPATCH_TMP}/homeboy-monorepo-releases.json"
  : > "${DISPATCH_TMP}/output"
  RUNNER_TEMP="${DISPATCH_TMP}" GITHUB_OUTPUT="${DISPATCH_TMP}/output" DRY_RUN="$2" \
    python3 "${ROOT_DIR}/scripts/release/run-monorepo-releases.py" released >/dev/null
  sed -n 's/^released-count=//p' "${DISPATCH_TMP}/output"
}
mixed='[{"component":"alpha","released":true,"version":"1.0.0","tag":"alpha-v1.0.0","bump":"minor","skipped_reason":""},{"component":"quiet","released":false,"version":"","tag":"","bump":"","skipped_reason":"no-releasable-commits"},{"component":"broken","released":false,"version":"","tag":"","bump":"","skipped_reason":"release-failed"}]'
expect "$(released_count "${mixed}" false)" 1 "only components actually released are dispatched"
expect "$(sed -n 's/^released=//p' "${DISPATCH_TMP}/output")" '[{"component":"alpha","version":"1.0.0","tag":"alpha-v1.0.0","bump":"minor"}]' "released list carries the dispatch payload fields"
expect "$(released_count "${mixed}" true)" 0 "a dry run never yields components to dispatch"
expect "$(released_count '[{"component":"quiet","released":false,"tag":""}]' false)" 0 "a no-op monorepo run yields nothing to dispatch"
rm -f "${DISPATCH_TMP}/homeboy-monorepo-releases.json"
: > "${DISPATCH_TMP}/output"
RUNNER_TEMP="${DISPATCH_TMP}" GITHUB_OUTPUT="${DISPATCH_TMP}/output" DRY_RUN=false \
  python3 "${ROOT_DIR}/scripts/release/run-monorepo-releases.py" released >/dev/null
expect "$(sed -n 's/^released-count=//p' "${DISPATCH_TMP}/output")" 0 "a missing aggregate file yields nothing to dispatch"

printf 'All release workflow checks passed.\n'
