#!/usr/bin/env bash

set -euo pipefail

ANNOTATIONS_DIR="${HOMEBOY_ANNOTATIONS_DIR:-}"
REPO="${GITHUB_REPOSITORY}"
ACTION_DIR="${GITHUB_ACTION_PATH}"

if [ -z "${ANNOTATIONS_DIR}" ] || [ -z "${PR_NUMBER}" ]; then
  echo "Skipping inline review — missing annotations dir or PR number"
  exit 0
fi

post_digest_review_fallback() {
  local digest_file="${HOMEBOY_FAILURE_DIGEST_FILE:-}"
  if [ -z "${digest_file}" ] || [ ! -f "${digest_file}" ]; then
    echo "No failure digest available for inline review fallback"
    return 0
  fi

  local review_body
  review_body=$(python3 - "$digest_file" <<'PY'
import sys
from pathlib import Path

digest_path = Path(sys.argv[1])
text = digest_path.read_text(encoding="utf-8", errors="replace").strip()

# Keep review body bounded so GitHub payloads stay predictable.
max_chars = 65000
if len(text) > max_chars:
    text = text[:max_chars] + "\n\n_Truncated for GitHub review payload size._"

print("## Homeboy Failure Digest")
print("")
print(text)
PY
)

  if [ -z "${review_body}" ]; then
    echo "Failure digest fallback produced empty review body"
    return 0
  fi

  if ! gh api "repos/${REPO}/pulls/${PR_NUMBER}/reviews" \
    --method POST \
    --field event="COMMENT" \
    --field body="${review_body}" > /dev/null 2>&1; then
    echo "::warning::Could not post digest fallback review"
    return 0
  fi

  echo "Posted fallback PR review from failure digest"
}

ANNOTATION_COUNT=$(find "${ANNOTATIONS_DIR}" -name "*.json" -type f 2>/dev/null | wc -l)
if [ "${ANNOTATION_COUNT}" -eq 0 ]; then
  echo "No annotation files found — skipping inline review"
  post_digest_review_fallback
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
python3 "${ACTION_DIR}/scripts/pr/find-related-files.py" \
  "$(pwd)" "${CHANGED_FILES_FILE}" > "${RELATED_FILES_FILE}" || true

RELATED_COUNT=$(wc -l < "${RELATED_FILES_FILE}" | tr -d ' ')
echo "Found ${RELATED_COUNT} related file(s)"

REVIEW_ARGS=("${ANNOTATIONS_DIR}" "${CHANGED_FILES_FILE}" "${PR_HEAD_SHA}")
if [ -s "${RELATED_FILES_FILE}" ]; then
  REVIEW_ARGS+=("${RELATED_FILES_FILE}")
fi

REVIEW_PAYLOAD=$(python3 "${ACTION_DIR}/scripts/pr/build-review.py" "${REVIEW_ARGS[@]}" 2>/dev/null || true)

rm -f "${CHANGED_FILES_FILE}" "${RELATED_FILES_FILE}"

if [ -z "${REVIEW_PAYLOAD}" ]; then
  echo "No annotations to post — skipping inline review"
  post_digest_review_fallback
  exit 0
fi

EXISTING_REVIEWS=$(gh api "repos/${REPO}/pulls/${PR_NUMBER}/reviews" \
  --jq '[.[] | select(.user.login == "github-actions[bot]" and (.body | test("Homeboy found|Collateral damage|Homeboy Failure Digest"))) | .id] | .[]' \
  2>/dev/null || true)

for REVIEW_ID in ${EXISTING_REVIEWS}; do
  echo "Dismissing previous review ${REVIEW_ID}..."
  gh api "repos/${REPO}/pulls/${PR_NUMBER}/reviews/${REVIEW_ID}/dismissals" \
    --method PUT \
    --field message="Superseded by new Homeboy review" \
    --field event="DISMISS" > /dev/null 2>&1 || true
done

if ! echo "${REVIEW_PAYLOAD}" | gh api "repos/${REPO}/pulls/${PR_NUMBER}/reviews" \
  --method POST \
  --input - > /dev/null 2>&1; then
  echo "::warning::Could not post inline review (likely restricted token for fork PR). Skipping inline review publish."
  exit 0
fi

COMMENT_COUNT=$(echo "${REVIEW_PAYLOAD}" | jq '.comments | length' 2>/dev/null || echo "0")
echo "Posted inline review with ${COMMENT_COUNT} comment(s)"
