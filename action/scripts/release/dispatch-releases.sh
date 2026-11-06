#!/usr/bin/env bash
#
# Send one post-release repository_dispatch per released component.
#
# Shared by both modes of the reusable release workflow so single-component
# and `components: all` callers get the same payload and auth:
#
#   RELEASES_JSON   JSON list of {component, version, tag, bump}. Monorepo mode
#                   passes the monorepo job's `released` output. When empty, a
#                   one-element list is built from COMPONENT/VERSION/TAG/BUMP
#                   (single-component mode).
#   DISPATCH_REPO   owner/repo to notify.
#   DISPATCH_EVENT  event_type.
#   DISPATCH_TOKEN  preferred token (contents:write on DISPATCH_REPO).
#   APP_TOKEN       fallback HOMEBOY_APP installation token scoped to the repo.
#   RUN_URL         link back to the releasing run.
#
# The payload `sha` is the peeled commit of each emitted tag, resolved from the
# local checkout (which must contain the tags). A failed dispatch does not stop
# the remaining ones; the script exits non-zero at the end instead.

set -euo pipefail

: "${DISPATCH_REPO:?DISPATCH_REPO is required}"
: "${DISPATCH_EVENT:?DISPATCH_EVENT is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"

releases="${RELEASES_JSON:-}"
if [ -z "${releases}" ]; then
  releases="$(jq -nc \
    --arg component "${COMPONENT:-}" \
    --arg version "${VERSION:-}" \
    --arg tag "${TAG:-}" \
    --arg bump "${BUMP:-}" \
    '[{component:$component,version:$version,tag:$tag,bump:$bump}]')"
fi

count="$(jq 'length' <<<"${releases}")"
if [ "${count}" -eq 0 ]; then
  echo "No released components; nothing to dispatch."
  exit 0
fi

if [ -n "${DISPATCH_TOKEN:-}" ]; then
  token="${DISPATCH_TOKEN}"
  source="DISPATCH_TOKEN"
elif [ -n "${APP_TOKEN:-}" ]; then
  token="${APP_TOKEN}"
  source="HOMEBOY_APP installation token scoped to ${DISPATCH_REPO}"
else
  echo "::error::No token can dispatch to ${DISPATCH_REPO}: GITHUB_TOKEN cannot send repository_dispatch cross-repo. Pass secrets.DISPATCH_TOKEN (contents:write on the target) or install the Homeboy App on the target repository."
  exit 1
fi

body_dir="${RUNNER_TEMP:-$(mktemp -d)}"
failed=0
for index in $(seq 0 $((count - 1))); do
  row="$(jq -c ".[${index}]" <<<"${releases}")"
  component="$(jq -r '.component // ""' <<<"${row}")"
  tag="$(jq -r '.tag // ""' <<<"${row}")"

  if [ -z "${tag}" ] || ! tag_sha="$(git rev-parse --verify --quiet "${tag}^{commit}")"; then
    echo "::error::Could not resolve released tag '${tag}' (${component}) to its commit SHA."
    failed=1
    continue
  fi

  body="${body_dir}/dispatch_body_${index}.json"
  jq -n \
    --argjson row "${row}" \
    --arg repository "${GITHUB_REPOSITORY}" \
    --arg sha "${tag_sha}" \
    --arg run_url "${RUN_URL:-}" \
    --arg event_type "${DISPATCH_EVENT}" \
    '{event_type:$event_type,client_payload:{component:($row.component // ""),repository:$repository,version:($row.version // ""),tag:($row.tag // ""),bump:($row.bump // ""),sha:$sha,run_url:$run_url}}' \
    > "${body}"

  echo "Dispatching ${DISPATCH_EVENT} for ${component} ${tag} to ${DISPATCH_REPO} using ${source}"
  cat "${body}"

  # Send the whole body as JSON. `gh api -F key=@file` posts the file
  # contents as a string, which GitHub rejects with HTTP 422
  # ("client_payload ... is not an object").
  if ! GH_TOKEN="${token}" gh api \
    --method POST \
    "repos/${DISPATCH_REPO}/dispatches" \
    --input "${body}"; then
    echo "::error::repository_dispatch for ${component} ${tag} to ${DISPATCH_REPO} failed."
    failed=1
  fi
done

exit "${failed}"
