#!/usr/bin/env bash

set -euo pipefail

ANNOTATIONS_DIR="${HOMEBOY_ANNOTATIONS_DIR:-}"
REPO="${GITHUB_REPOSITORY}"
ACTION_DIR="${GITHUB_ACTION_PATH}"

if [ -z "${ANNOTATIONS_DIR}" ] || [ -z "${PR_NUMBER}" ]; then
  echo "Skipping inline review — missing annotations dir or PR number"
  exit 0
fi

dismiss_existing_bot_reviews() {
  local existing_reviews
  existing_reviews=$(gh api "repos/${REPO}/pulls/${PR_NUMBER}/reviews" \
    --jq '[.[] | select(.user.login == "github-actions[bot]" and (.body | test("Homeboy found"))) | .id] | .[]' \
    2>/dev/null || true)

  for review_id in ${existing_reviews}; do
    echo "Dismissing previous review ${review_id}..."
    gh api "repos/${REPO}/pulls/${PR_NUMBER}/reviews/${review_id}/dismissals" \
      --method PUT \
      --field message="Superseded by updated Homeboy review" \
      --field event="DISMISS" > /dev/null 2>&1 || true
  done
}

ANNOTATION_COUNT=$(find "${ANNOTATIONS_DIR}" -name "*.json" -type f 2>/dev/null | wc -l)
if [ "${ANNOTATION_COUNT}" -eq 0 ]; then
  echo "No annotation files found — skipping inline review"
  dismiss_existing_bot_reviews
  exit 0
fi

echo "Found ${ANNOTATION_COUNT} annotation file(s):"
ls -la "${ANNOTATIONS_DIR}"/*.json 2>/dev/null || true

# Only post annotations on files actually changed in the PR.
# Collateral damage (effects on files that reference changed symbols) is
# handled by the audit engine via --changed-since, not by the review layer.
CHANGED_FILES_FILE=$(mktemp)
gh api "repos/${REPO}/pulls/${PR_NUMBER}/files" \
  --paginate --jq '.[].filename' > "${CHANGED_FILES_FILE}" 2>/dev/null || true

if [ ! -s "${CHANGED_FILES_FILE}" ]; then
  echo "Could not fetch PR changed files — skipping inline review"
  rm -f "${CHANGED_FILES_FILE}"
  exit 0
fi

# Build review payload — changed files only, no collateral damage
REVIEW_PAYLOAD=$(python3 "${ACTION_DIR}/scripts/pr/build-review.py" \
  "${ANNOTATIONS_DIR}" "${CHANGED_FILES_FILE}" "${PR_HEAD_SHA}" 2>/dev/null || true)

rm -f "${CHANGED_FILES_FILE}"

if [ -z "${REVIEW_PAYLOAD}" ]; then
  echo "No annotations in changed files — skipping inline review"
  dismiss_existing_bot_reviews
  exit 0
fi

dismiss_existing_bot_reviews

if ! echo "${REVIEW_PAYLOAD}" | gh api "repos/${REPO}/pulls/${PR_NUMBER}/reviews" \
  --method POST \
  --input - > /dev/null 2>&1; then
  echo "::warning::Could not post inline review (likely restricted token for fork PR). Skipping inline review publish."
  exit 0
fi

COMMENT_COUNT=$(echo "${REVIEW_PAYLOAD}" | jq '.comments | length' 2>/dev/null || echo "0")
echo "Posted inline review with ${COMMENT_COUNT} comment(s)"
