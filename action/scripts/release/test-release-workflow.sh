#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKFLOW="${ROOT_DIR}/.github/workflows/self-release.yml"
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
MAJOR_TAG_HOOK="${ROOT_DIR}/scripts/release/update-major-tag.sh"

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

assert_not_contains '^  create-release:' "${WORKFLOW}" "release workflow has no duplicate GitHub Release job"
assert_not_contains 'gh release create' "${WORKFLOW}" "release workflow does not shell out to gh release create"
assert_not_contains 'docs/CHANGELOG.md' "${WORKFLOW}" "release workflow does not parse changelog notes"

# GitHub-hosted workflow dependencies must use Node 24-capable action majors (#321).
assert_count 'actions/checkout@v6' '17' "${CI_WORKFLOW}" "reusable CI workflow uses checkout v6 for every checkout"
assert_count 'actions/cache@v5' '1' "${CI_WORKFLOW}" "reusable CI workflow owns one candidate build cache"
assert_count 'actions/cache@v5' '3' "${ROOT_DIR}/action.yml" "composite action owns release, source-build, and extension caches"
assert_count 'actions/upload-artifact@v7' '8' "${CI_WORKFLOW}" "reusable CI workflow uses artifact upload v7"
assert_count 'actions/download-artifact@v7' '8' "${CI_WORKFLOW}" "reusable CI workflow uses artifact download v7"
assert_count 'actions/create-github-app-token@v3' '3' "${CI_WORKFLOW}" "reusable CI workflow uses app-token v3"
assert_not_contains 'actions/checkout@v4' "${CI_WORKFLOW}" "reusable CI workflow has no checkout v4 references"
assert_not_contains 'actions/cache@v4' "${CI_WORKFLOW}" "reusable CI workflow has no cache v4 references"
assert_not_contains 'actions/\(upload\|download\)-artifact@v4' "${CI_WORKFLOW}" "reusable CI workflow has no artifact v4 references"
assert_not_contains 'actions/create-github-app-token@v2' "${CI_WORKFLOW}" "reusable CI workflow has no app-token v2 references"
assert_not_contains 'merge-multiple: true' "${CI_WORKFLOW}" "reusable CI workflow never merges competing binary artifacts"
assert_contains 'actions: read' "${CI_WORKFLOW}" "reusable CI workflow can download current-run artifacts"
assert_count 'actions/checkout@v6' '3' "${WORKFLOW}" "release workflow uses checkout v6 for every checkout"
assert_count 'actions/create-github-app-token@v3' '1' "${WORKFLOW}" "release workflow uses app-token v3"
assert_not_contains 'actions/checkout@v4' "${WORKFLOW}" "release workflow has no checkout v4 references"
assert_not_contains 'actions/create-github-app-token@v2' "${WORKFLOW}" "release workflow has no app-token v2 references"
assert_contains 'actions/checkout@v6' "${SELF_TEST_WORKFLOW}" "self-test workflow uses checkout v6"
assert_not_contains 'actions/checkout@v4' "${SELF_TEST_WORKFLOW}" "self-test workflow has no checkout v4 references"
assert_count 'actions/upload-artifact@v7' '4' "${ROOT_DIR}/action.yml" "composite action uses artifact upload v7"
assert_count 'actions/download-artifact@v7' '1' "${ROOT_DIR}/action.yml" "composite action uses artifact download v7"
assert_not_contains 'actions/\(upload\|download\)-artifact@v4' "${ROOT_DIR}/action.yml" "composite action has no artifact v4 references"

# Releasing moves the floating v2 tag, so the action's own shell tests must
# gate publication rather than running advisory-only (or not at all).
assert_contains 'uses: ./.github/workflows/self-test.yml' "${WORKFLOW}" "release workflow runs the action self-test suite"
assert_contains '      - self-test' "${WORKFLOW}" "release job is gated on the self-test suite"
assert_contains 'uses: ./.github/workflows/consumer-contract.yml' "${WORKFLOW}" "release workflow runs the GitHub consumer contract probe"
assert_contains '      - consumer-contract' "${WORKFLOW}" "release job is gated on the GitHub consumer contract probe"
assert_contains 'bash scripts/run-tests.sh' "${SELF_TEST_WORKFLOW}" "self-test workflow runs every action shell test"
assert_contains 'pull_request' "${SELF_TEST_WORKFLOW}" "self-test workflow also guards pull requests"

assert_contains 'uses: ./' "${WORKFLOW}" "release workflow uses checked-out action code for self-release"
assert_not_contains 'Extra-Chill/homeboy-action@v2' "${WORKFLOW}" "release workflow is not bootstrapped from the stale floating v2 channel"
assert_contains 'commands: release' "${WORKFLOW}" "release workflow delegates to homeboy-action release command"
assert_count 'release-skip-github-release' '1' "${WORKFLOW}" "release workflow only skips GitHub Release creation during dry-run check"
assert_contains 'runner.temp }}/homeboy-release-last-failed' "${WORKFLOW}" "release failure cache lives outside checkout"
assert_not_contains 'path: .release-last-failed' "${WORKFLOW}" "release failure cache is not restored into checkout"
assert_not_contains '< .release-last-failed' "${WORKFLOW}" "release failure marker is not read from checkout"
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
assert_contains 'bash scripts/release/update-major-tag.sh' "${ROOT_DIR}/homeboy.json" "post-release delegates v2 identity checks to the tested hook"
assert_contains 'release-verify-github-release:' "${ROOT_DIR}/action.yml" "action exposes GitHub Release verification toggle"
assert_contains 'HOMEBOY_VERIFY_GITHUB_RELEASE: ${{ inputs.release-verify-github-release }}' "${ROOT_DIR}/action.yml" "action passes verification toggle to release script"
assert_contains 'run-release-with-liveness.sh' "${ROOT_DIR}/action.yml" "action routes releases through the shared liveness boundary"
assert_contains 'HOMEBOY_ACTION_PHASE_HEARTBEAT_SECONDS: ${{ inputs.phase-heartbeat-seconds }}' "${ROOT_DIR}/action.yml" "action passes heartbeat settings to release phases"
assert_contains 'HOMEBOY_ACTION_PHASE_BUDGET_SECONDS: ${{ inputs.phase-budget-seconds }}' "${ROOT_DIR}/action.yml" "action passes phase budgets to release phases"
assert_contains 'id: artifact-names' "${ROOT_DIR}/action.yml" "action computes matrix-safe artifact names"
assert_contains 'PORTABLE_ID_VALUE: ${{ steps.read-config.outputs.portable-id }}' "${ROOT_DIR}/action.yml" "artifact names include component identity"
assert_contains 'PHP_VALUE: ${{ steps.detect-env.outputs.portable-php }}' "${ROOT_DIR}/action.yml" "artifact names include PHP matrix identity"
assert_contains 'NODE_VALUE: ${{ steps.detect-env.outputs.portable-node }}' "${ROOT_DIR}/action.yml" "artifact names include Node matrix identity"
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
assert_contains "github.event_name == 'workflow_dispatch'" "${WORKFLOW}" "release workflow detects manual dispatch for the marker bypass"
assert_contains 'IS_MANUAL_DISPATCH' "${WORKFLOW}" "release workflow gates the failed-SHA skip on non-dispatch runs"
assert_contains 'release-last-failed-${{ github.ref_name }}-${{ github.sha }}-root-${{ steps.release-check.outputs.tooling-identity }}' "${WORKFLOW}" "failure-cache restore key includes root component and tooling identity"
assert_contains 'release-last-failed-${{ github.ref_name }}-${{ github.sha }}-root-${{ needs.check.outputs.tooling-identity }}' "${WORKFLOW}" "failure-cache save key includes root component and tooling identity"
assert_not_contains 'restore-keys:' "${WORKFLOW}" "failure cache no longer falls back to a stale-tooling marker via restore-keys"
assert_contains 'git/ref/tags/${release_tag}' "${MAJOR_TAG_HOOK}" "post-release hook reads the pushed release tag ref"
assert_contains 'release_ref_sha' "${MAJOR_TAG_HOOK}" "post-release hook points v2 at the pushed release tag target"
assert_contains 'head_sha.*main_sha' "${MAJOR_TAG_HOOK}" "post-release hook authorizes the remote main identity instead of a local branch label"
assert_not_contains 'gh api.*||' "${MAJOR_TAG_HOOK}" "post-release hook does not swallow failed v2 tag updates"
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
assert_contains 'TAG_SHA: ${{ steps.tag-sha.outputs.sha }}' "${REUSABLE_RELEASE}" "dispatch receives the released tag SHA"
assert_contains 'ref: ${{ needs.release.outputs.release-tag }}' "${REUSABLE_RELEASE}" "dispatch checks out the released tag with repository credentials"
assert_contains 'git rev-parse --verify "${TAG}^{commit}"' "${REUSABLE_RELEASE}" "dispatch resolves lightweight and annotated tags"
assert_not_contains 'git ls-remote "https://github.com/${GITHUB_REPOSITORY}.git"' "${REUSABLE_RELEASE}" "dispatch does not resolve private tags anonymously"
assert_contains 'prepared-ref' "${ROOT_DIR}/scripts/release/test-resolve-prepared-ref.sh" "prepared-ref behavior has a focused test"

python3 - "${REUSABLE_RELEASE}" <<'PY'
import sys

workflow = open(sys.argv[1]).read()
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
assert_contains "needs.release.outputs.released == 'true' && inputs.dispatch-repo != '' && inputs.dry-run != true" "${REUSABLE_RELEASE}" "dispatch job is gated on a real successful release with a target and no dry-run"
assert_contains '"repos/${DISPATCH_REPO}/dispatches"' "${REUSABLE_RELEASE}" "dispatch job posts a repository_dispatch to the target"
assert_contains 'GITHUB_TOKEN cannot send repository_dispatch cross-repo' "${REUSABLE_RELEASE}" "dispatch job fails closed without a cross-repo-capable token"
assert_contains 'contents:write on the target' "${REUSABLE_RELEASE}" "dispatch documentation uses contents write permission"
assert_contains 'value: ${{ jobs.dispatch.result == '"'"'success'"'"' }}' "${REUSABLE_RELEASE}" "reusable release workflow re-exports dispatched"
assert_contains 'component: ${{ steps.release.outputs.component }}' "${REUSABLE_RELEASE}" "release job exposes the portable component id for the payload"
assert_contains 'dispatch-repo: example-org/example-deploy' "${DISPATCH_CONSUMER_FIXTURE}" "dispatch consumer fixture sets a dispatch target"
assert_contains 'dry-run: true' "${DISPATCH_CONSUMER_FIXTURE}" "dispatch consumer fixture stays dry-run so it can never dispatch"
assert_contains 'secrets: inherit' "${DISPATCH_CONSUMER_FIXTURE}" "dispatch consumer fixture inherits secrets"

printf 'All release workflow checks passed.\n'
