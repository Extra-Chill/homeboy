#!/usr/bin/env bash
#
# Reusable-workflow pin validator.
#
# `uses: owner/repo/.github/workflows/file.yml@<ref>` resolves COMMIT SHAs only.
# An annotated tag's ref object is a *tag* object, not a commit, so pinning the
# SHA that `git/refs/tags/<tag>` reports fails the whole workflow at startup —
# no jobs scheduled, no logs, just "This run likely failed because of a workflow
# file issue".
#
# That is not hypothetical. On 2026-08-12 PR #12152 pinned a6ff2f5d, the tag
# object for homeboy-action v2.11.21, and every pull request's CI died at
# startup until #12153 repinned the dereferenced commit 82033fe6. It passed
# local review because `git show <tagobj>:path` and `git rev-parse` both
# dereference annotated tags transparently: the YAML parsed, the file content
# was correct, and only GitHub's resolver disagreed.
#
# Pin `git rev-parse v<tag>^{commit}`, and let this catch it when someone does
# not. Consumed by the `homeboy / Required Gates Declaration` job.
#
# Requires network and a token. Unreadable refs are reported as `unverified`
# and never treated as valid, but only `tag` — a positively identified wrong
# object type — fails the build.

set -euo pipefail

ci_workflow="${ACTION_PIN_WORKFLOW:-.github/workflows/ci.yml}"

if [ ! -f "${ci_workflow}" ]; then
  echo "action-pin: ${ci_workflow} not found" >&2
  exit 1
fi

mapfile -t pins < <(grep -oE 'uses: [A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/\.github/workflows/[A-Za-z0-9_.-]+@[0-9a-f]{40}' "${ci_workflow}" | sed 's/^uses: //')

if [ "${#pins[@]}" -eq 0 ]; then
  echo "action-pin: no SHA-pinned reusable workflows in ${ci_workflow}; nothing to verify"
  exit 0
fi

status=0

for pin in "${pins[@]}"; do
  target="${pin%@*}"
  sha="${pin##*@}"
  repo="$(printf '%s' "${target}" | cut -d/ -f1-2)"

  # `git/commits/<sha>` is the discriminator: it 404s for a tag object and
  # succeeds only for a commit. Asking `git/refs` would re-introduce the exact
  # dereferencing ambiguity this script exists to remove.
  if resolved="$(gh api "repos/${repo}/git/commits/${sha}" --jq '.sha' 2>/dev/null)" && [ "${resolved}" = "${sha}" ]; then
    echo "action-pin: ${repo}@${sha} is a commit"
    continue
  fi

  if object_type="$(gh api "repos/${repo}/git/tags/${sha}" --jq '.object.type' 2>/dev/null)"; then
    commit="$(gh api "repos/${repo}/git/tags/${sha}" --jq '.object.sha' 2>/dev/null || echo unknown)"
    echo "::error::action-pin: ${repo}@${sha} is an ANNOTATED TAG object, not a commit (${object_type} -> ${commit}). \`uses:\` cannot resolve it and CI will fail at startup with zero jobs. Pin the dereferenced commit ${commit} instead." >&2
    status=1
    continue
  fi

  echo "::warning::action-pin: ${repo}@${sha} could not be read as either a commit or a tag object; reporting unverified rather than valid." >&2
done

exit "${status}"
