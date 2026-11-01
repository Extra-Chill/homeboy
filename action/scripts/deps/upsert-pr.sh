#!/usr/bin/env bash
# Rebuild the automation-owned branch from the current default branch each run.
set -euo pipefail
: "${GH_TOKEN:?}" "${BASE_BRANCH:?}" "${GATE_OUTCOME:?}" "${GITHUB_REPOSITORY:?}" "${RUNNER_TEMP:?}"
branch=homeboy/deps-refresh
body="${RUNNER_TEMP}/deps-refresh.md"
printf '\nGates: **%s**. [Workflow run](%s/%s/actions/runs/%s).\n' \
  "$GATE_OUTCOME" "${GITHUB_SERVER_URL:-https://github.com}" "$GITHUB_REPOSITORY" "${GITHUB_RUN_ID:?}" >> "$body"

# persist-credentials:false matches release.yml; authenticate only these git
# invocations, never in the remote URL or the on-disk git configuration.
export GIT_CONFIG_COUNT=1
export GIT_CONFIG_KEY_0=http.https://github.com/.extraheader
export GIT_CONFIG_VALUE_0="AUTHORIZATION: basic $(printf 'x-access-token:%s' "$GH_TOKEN" | base64 -w0)"
base_sha="$(git rev-parse HEAD)"
remote_base="$(git ls-remote origin "refs/heads/$BASE_BRANCH" | cut -f1)"
if [[ "$base_sha" != "$remote_base" ]]; then
  echo '::error::Default branch advanced during refresh; retry against the new tip.'
  exit 1
fi
old_sha="$(git ls-remote origin "refs/heads/$branch" | cut -f1)"
git config user.name 'homeboy[bot]'
git config user.email 'homeboy[bot]@users.noreply.github.com'
git switch -C "$branch"
git add -A
if git diff --cached --quiet; then
  echo 'No dependency changes to propose.'
  exit 0
fi
git commit -m 'chore(deps): refresh dependency stack'
git push --force-with-lease="refs/heads/$branch:$old_sha" origin "HEAD:refs/heads/$branch"
pr="$(gh pr list --repo "$GITHUB_REPOSITORY" --head "$branch" --base "$BASE_BRANCH" --state open --json number --jq '.[0].number // empty')"
if [[ -n "$pr" ]]; then
  gh pr edit "$pr" --repo "$GITHUB_REPOSITORY" --title 'chore(deps): refresh dependency stack' --body-file "$body"
else
  gh pr create --repo "$GITHUB_REPOSITORY" --head "$branch" --base "$BASE_BRANCH" \
    --title 'chore(deps): refresh dependency stack' --body-file "$body"
fi
