#!/usr/bin/env bash
#
# Detect a *stranded prepared release*: a tag this pipeline already prepared
# (version bumped, changelog written, tag pushed) whose GitHub Release is
# missing entirely or is still sitting as a Draft.
#
# Why this exists (issue #10441)
# ------------------------------
# `release.yml` prepares and pushes the tag in one job and publishes the
# GitHub Release in later jobs. A transient failure between those two points
# (v0.320.0: `gh release upload` lost a `gh api release metadata` call) leaves
# the tag behind with no published release. The workflow already knows how to
# finish such a release — `recovery-release=true` skips the quality gates and
# republishes from the tag — but until now that path only fired when either
#
#   * a human passed `workflow_dispatch` `release_tag`, or
#   * `release --dry-run` at HEAD reported `bump-type=recovery`, which requires
#     HEAD to still be sitting *on* the prepared tag.
#
# main moves within minutes, so the second condition evaporates and the
# stranded tag is never revisited. This script makes the detection
# **HEAD-independent**: it asks git and the GitHub API what is stranded and
# never looks at `github.sha`.
#
# Safety properties
# -----------------
# 1. Only tags this pipeline prepared are eligible. A candidate must have the
#    exact release tag shape (`vX.Y.Z`, the same regex `release.yml` validates
#    dispatch input against) *and* the tagged commit's subject must be exactly
#    `release: <tag>` — the subject `homeboy release` writes. A Draft a human
#    created for some other purpose cannot satisfy both.
# 2. The tagged commit must be reachable from the release branch. A tag parked
#    on an unmerged side branch is not this pipeline's to publish.
# 3. In-flight releases are held, not recovered *and not overtaken*.
#    `cargo-dist` creates the GitHub Release as a Draft and publishes it
#    minutes later, so a healthy running release legitimately looks stranded
#    while it runs. A tag younger than STRANDED_MIN_AGE_MINUTES therefore
#    produces `hold-reason` — this run neither recovers it nor prepares a fresh
#    release on top of it. Waiting one cycle is strictly better than either
#    double-publishing a live release or burying a fresh one over a tag that
#    turns out to be stranded.
# 4. Scanning stops at the first *published* release. Recovery is confined to
#    the contiguous run of unpublished tags at the head of the tag list.
#    Resurrecting an ancient orphan sitting below a published release would
#    ship stale artifacts and disturb `latest`; that is a human decision.
# 5. Oldest-first. When several tags are stranded the lowest version is chosen
#    so releases publish in version order — later versions' notes and changelog
#    ranges assume their predecessors shipped, and GitHub derives "latest" from
#    the newest published release. One recovery per run; the next push drains
#    the next one.
# 6. Bounded retries. A tag already attempted MAX_RECOVERY_ATTEMPTS times is
#    refused with an error annotation and the run falls through to a normal
#    fresh release, so an unrecoverable orphan cannot block delivery forever
#    and cannot spin recovery on every push.
#
# Env:
#   GH_TOKEN                    - required, for `gh release list`
#   GITHUB_OUTPUT               - required
#   RECOVERY_ATTEMPTS_FILE      - optional marker file: "<tag> <count>" per line
#   MAX_RECOVERY_ATTEMPTS       - default 3
#   STRANDED_SCAN_LIMIT         - default 20 newest tags
#   STRANDED_MIN_AGE_MINUTES    - default 45 (release pipeline runs ~27m)
#
# Outputs (GITHUB_OUTPUT):
#   stranded-tag      - tag to recover, or empty
#   stranded-version  - that tag without the leading "v", or empty
#   stranded-attempts - failed recovery attempts already recorded for that tag
#   hold-reason       - non-empty when this run must not release at all

set -euo pipefail

: "${GITHUB_OUTPUT:?GITHUB_OUTPUT must be set}"

SCAN_LIMIT="${STRANDED_SCAN_LIMIT:-20}"
MIN_AGE_MINUTES="${STRANDED_MIN_AGE_MINUTES:-45}"
MAX_ATTEMPTS="${MAX_RECOVERY_ATTEMPTS:-3}"
ATTEMPTS_FILE="${RECOVERY_ATTEMPTS_FILE:-}"

TAG_SHAPE='^v[0-9]+\.[0-9]+\.[0-9]+$'

emit() {
  printf '%s=%s\n' "$1" "$2" >> "${GITHUB_OUTPUT}"
}

finish() {
  emit 'stranded-tag' "${1:-}"
  emit 'stranded-version' "${2:-}"
  emit 'stranded-attempts' "${3:-0}"
  emit 'hold-reason' "${4:-}"
  exit 0
}

recorded_attempts() {
  local tag="$1"
  if [ -z "${ATTEMPTS_FILE}" ] || [ ! -f "${ATTEMPTS_FILE}" ]; then
    printf '0'
    return 0
  fi
  awk -v tag="${tag}" '$1 == tag { count = $2 } END { printf "%d", count + 0 }' "${ATTEMPTS_FILE}"
}

RELEASES_JSON="$(mktemp)"
RELEASES_ERR="$(mktemp)"
trap 'rm -f "${RELEASES_JSON}" "${RELEASES_ERR}"' EXIT

# One API call answers the question for every candidate. A failed call must
# never read as "nothing is published" — that would republish healthy releases
# — so a transient API error simply skips detection for this run.
if ! gh release list --limit 100 --json tagName,isDraft > "${RELEASES_JSON}" 2> "${RELEASES_ERR}"; then
  echo "::warning::Could not list GitHub Releases; skipping stranded-release detection this run"
  sed 's/^/gh: /' "${RELEASES_ERR}" >&2 || true
  finish '' '' 0 ''
fi

now_epoch="$(date +%s)"
min_age_seconds=$(( MIN_AGE_MINUTES * 60 ))

stranded_tags=()
stranded_reasons=()
hold_reason=''

while IFS= read -r tag; do
  [ -n "${tag}" ] || continue

  if [[ ! "${tag}" =~ ${TAG_SHAPE} ]]; then
    continue
  fi

  commit="$(git rev-parse -q --verify "${tag}^{commit}" 2>/dev/null || true)"
  if [ -z "${commit}" ]; then
    continue
  fi

  # Provenance: only tags whose commit is a Homeboy release commit are ours.
  subject="$(git log -1 --format=%s "${commit}")"
  if [ "${subject}" != "release: ${tag}" ]; then
    echo "::notice::Ignoring tag ${tag} for recovery: commit subject is '${subject}', not 'release: ${tag}'"
    continue
  fi

  # Provenance: the tag must live on the branch this workflow releases from.
  if ! git merge-base --is-ancestor "${commit}" HEAD; then
    echo "::notice::Ignoring tag ${tag} for recovery: not reachable from the release branch"
    continue
  fi

  entry="$(jq -c --arg tag "${tag}" 'map(select(.tagName == $tag)) | first // empty' "${RELEASES_JSON}")"

  if [ -n "${entry}" ] && [ "$(jq -r '.isDraft' <<< "${entry}")" != "true" ]; then
    # First published release found. Everything older is either published or a
    # historical orphan that must not be resurrected automatically.
    break
  fi

  tag_epoch="$(git log -1 --format=%ct "${commit}")"
  age_seconds=$(( now_epoch - tag_epoch ))
  if [ "${age_seconds}" -lt "${min_age_seconds}" ]; then
    hold_reason="tag ${tag} has no published release yet but is only $(( age_seconds / 60 ))m old (< ${MIN_AGE_MINUTES}m); its release pipeline may still be running"
    break
  fi

  if [ -z "${entry}" ]; then
    stranded_reasons+=('no GitHub Release')
  else
    stranded_reasons+=('Draft GitHub Release')
  fi
  stranded_tags+=("${tag}")
done < <(git tag --list --sort=-v:refname | head -n "${SCAN_LIMIT}")

for index in "${!stranded_tags[@]}"; do
  echo "::warning::Stranded prepared release: ${stranded_tags[${index}]} (${stranded_reasons[${index}]})"
done

if [ -n "${hold_reason}" ]; then
  # Holding beats both alternatives: recovering could double-publish a live
  # release, and preparing a fresh release would bury the tag under a newer
  # published one where the contiguous-window scan can never see it again.
  echo "::notice::Holding this release run — ${hold_reason}"
  finish '' '' 0 "${hold_reason}"
fi

if [ "${#stranded_tags[@]}" -eq 0 ]; then
  finish '' '' 0 ''
fi

# Oldest-first: the scan is newest-first, so the last entry is the lowest version.
last_index=$(( ${#stranded_tags[@]} - 1 ))
target="${stranded_tags[${last_index}]}"
attempts="$(recorded_attempts "${target}")"

if [ "${attempts}" -ge "${MAX_ATTEMPTS}" ]; then
  echo "::error::Stranded release ${target} has failed automatic recovery ${attempts} time(s) (limit ${MAX_ATTEMPTS}); not retrying. Recover manually with a workflow_dispatch run using release_tag=${target}, or delete the tag if it should not ship. Fresh releases continue so delivery is not blocked."
  finish '' '' "${attempts}" ''
fi

echo "::notice::Selecting stranded release ${target} for automatic recovery (attempt $(( attempts + 1 )) of ${MAX_ATTEMPTS})"
finish "${target}" "${target#v}" "${attempts}"
