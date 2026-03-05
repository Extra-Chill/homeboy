#!/usr/bin/env bash

set -euo pipefail

ANNOTATIONS_DIR="${HOMEBOY_ANNOTATIONS_DIR:-}"
REPO="${GITHUB_REPOSITORY}"
ACTION_DIR="${GITHUB_ACTION_PATH}"

if [ -z "${ANNOTATIONS_DIR}" ] || [ -z "${PR_NUMBER}" ]; then
  echo "Skipping inline review — missing annotations dir or PR number"
  exit 0
fi

ANNOTATION_COUNT=$(find "${ANNOTATIONS_DIR}" -name "*.json" -type f 2>/dev/null | wc -l)
if [ "${ANNOTATION_COUNT}" -eq 0 ]; then
  echo "No annotation files found — skipping inline review"
  exit 0
fi

echo "Found ${ANNOTATION_COUNT} annotation file(s):"
ls -la "${ANNOTATIONS_DIR}"/*.json 2>/dev/null || true

CHANGED_FILES_FILE=$(mktemp)
gh api "repos/${REPO}/pulls/${PR_NUMBER}/files" \
  --paginate --jq '.[].filename' > "${CHANGED_FILES_FILE}" 2>/dev/null || true

if [ ! -s "${CHANGED_FILES_FILE}" ]; then
  echo "Could not fetch PR changed files — skipping inline review"
  rm -f "${CHANGED_FILES_FILE}"
  exit 0
fi

RELATED_FILES_FILE=$(mktemp)
echo "Tracing symbol references from changed files..."
python3 "${ACTION_DIR}/scripts/find-related-files.py" \
  "$(pwd)" "${CHANGED_FILES_FILE}" > "${RELATED_FILES_FILE}" || true

RELATED_COUNT=$(wc -l < "${RELATED_FILES_FILE}" | tr -d ' ')
echo "Found ${RELATED_COUNT} related file(s)"

REVIEW_ARGS=("${ANNOTATIONS_DIR}" "${CHANGED_FILES_FILE}" "${PR_HEAD_SHA}")
if [ -s "${RELATED_FILES_FILE}" ]; then
  REVIEW_ARGS+=("${RELATED_FILES_FILE}")
fi

REVIEW_PAYLOAD=$(python3 "${ACTION_DIR}/scripts/build-review.py" "${REVIEW_ARGS[@]}" 2>/dev/null || true)

rm -f "${CHANGED_FILES_FILE}" "${RELATED_FILES_FILE}"

if [ -z "${REVIEW_PAYLOAD}" ]; then
  echo "No annotations to post — skipping inline review"
  exit 0
fi

EXISTING_REVIEWS=$(gh api "repos/${REPO}/pulls/${PR_NUMBER}/reviews" \
  --jq '[.[] | select(.user.login == "github-actions[bot]" and (.body | test("Homeboy found|Collateral damage"))) | .id] | .[]' \
  2>/dev/null || true)

for REVIEW_ID in ${EXISTING_REVIEWS}; do
  echo "Dismissing previous review ${REVIEW_ID}..."
  gh api "repos/${REPO}/pulls/${PR_NUMBER}/reviews/${REVIEW_ID}/dismissals" \
    --method PUT \
    --field message="Superseded by new Homeboy review" \
    --field event="DISMISS" > /dev/null 2>&1 || true
done

echo "${REVIEW_PAYLOAD}" | gh api "repos/${REPO}/pulls/${PR_NUMBER}/reviews" \
  --method POST \
  --input - > /dev/null 2>&1

COMMENT_COUNT=$(echo "${REVIEW_PAYLOAD}" | jq '.comments | length' 2>/dev/null || echo "0")
echo "Posted inline review with ${COMMENT_COUNT} comment(s)"
