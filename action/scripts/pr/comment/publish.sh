#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/pr/comment/lib.sh"

merge_comment_payload() {
  local comments_file="$1"
  local section_file="$2"
  local tooling_file="$3"

  python3 "${GITHUB_ACTION_PATH}/scripts/pr/merge-pr-comment.py" \
    "${comments_file}" \
    "${COMMENT_KEY}" \
    "${COMP_ID}" \
    "${SECTION_KEY}" \
    "${section_file}" \
    "${tooling_file}" 2>/dev/null || true
}

publish_comment_body() {
  local comment_body="$1"
  local existing_comment_id="$2"

  if [ -n "${existing_comment_id}" ]; then
    echo "Updating shared comment ${existing_comment_id}..." >&2
    if ! gh api "repos/${REPO}/issues/comments/${existing_comment_id}" \
      --method PATCH \
      --field body="${comment_body}" > /dev/null 2>&1; then
      echo "::warning::Could not update PR comment (likely restricted token for fork PR). Skipping comment publish."
      return 1
    fi
    printf '%s\n' "${existing_comment_id}"
    return 0
  fi

  echo "Creating shared comment..." >&2
  local create_response
  create_response=$(gh api "repos/${REPO}/issues/${PR_NUMBER}/comments" \
    --method POST \
    --field body="${comment_body}" 2>/dev/null || true)
  if [ -z "${create_response}" ]; then
    echo "::warning::Could not create PR comment (likely restricted token for fork PR). Skipping comment publish."
    return 1
  fi

  printf '%s\n' "$(printf '%s' "${create_response}" | jq -r '.id // empty')"
}

delete_comment_ids() {
  local merge_result="$1"

  printf '%s' "${merge_result}" | jq -r '.delete_ids[]?' | while IFS= read -r comment_id; do
    if [ -n "${comment_id}" ]; then
      echo "Deleting superseded comment ${comment_id}..."
      gh api "repos/${REPO}/issues/comments/${comment_id}" --method DELETE > /dev/null 2>&1 || true
    fi
  done
}

consolidate_canonical_comment() {
  local section_file="$1"
  local final_comments_file
  local final_tooling_file
  local final_merge_result
  local canonical_comment_id
  local final_comment_body

  final_comments_file=$(mktemp)
  final_tooling_file=$(mktemp)
  append_tooling_json "${final_tooling_file}"

  if gh api "repos/${REPO}/issues/${PR_NUMBER}/comments" > "${final_comments_file}" 2>/dev/null; then
    final_merge_result=$(merge_comment_payload "${final_comments_file}" "${section_file}" "${final_tooling_file}")

    if [ -n "${final_merge_result}" ]; then
      canonical_comment_id=$(printf '%s' "${final_merge_result}" | jq -r '.comment_id // empty')
      final_comment_body=$(printf '%s' "${final_merge_result}" | jq -r '.body')
      if [ -n "${canonical_comment_id}" ] && [ -n "${final_comment_body}" ]; then
        gh api "repos/${REPO}/issues/comments/${canonical_comment_id}" \
          --method PATCH \
          --field body="${final_comment_body}" > /dev/null 2>&1 || true
      fi

      printf '%s' "${final_merge_result}" | jq -r '.delete_ids[]?' | while IFS= read -r comment_id; do
        if [ -n "${comment_id}" ] && [ "${comment_id}" != "${canonical_comment_id:-}" ]; then
          echo "Deleting duplicate shared comment ${comment_id}..."
          gh api "repos/${REPO}/issues/comments/${comment_id}" --method DELETE > /dev/null 2>&1 || true
        fi
      done
    fi
  fi

  rm -f "${final_comments_file}" "${final_tooling_file}"
}

publish_pr_comment() {
  local section_file comments_file tooling_file merge_result comment_body existing_comment_id posted_comment_id

  section_file=$(mktemp)
  comments_file=$(mktemp)
  tooling_file=$(mktemp)

  printf '%s' "${SECTION_BODY}" > "${section_file}"
  append_tooling_json "${tooling_file}"

  if ! gh api "repos/${REPO}/issues/${PR_NUMBER}/comments" > "${comments_file}" 2>/dev/null; then
    echo "::warning::Could not read PR comments (likely restricted token for fork PR). Skipping comment publish."
    rm -f "${section_file}" "${comments_file}" "${tooling_file}"
    return 0
  fi

  merge_result=$(merge_comment_payload "${comments_file}" "${section_file}" "${tooling_file}")
  rm -f "${comments_file}" "${tooling_file}"

  if [ -z "${merge_result}" ]; then
    echo "::warning::Could not merge PR comment content. Skipping comment publish."
    rm -f "${section_file}"
    return 0
  fi

  comment_body=$(printf '%s' "${merge_result}" | jq -r '.body')
  existing_comment_id=$(printf '%s' "${merge_result}" | jq -r '.comment_id // empty')
  posted_comment_id="$(publish_comment_body "${comment_body}" "${existing_comment_id}")" || {
    rm -f "${section_file}"
    return 0
  }

  if [ -n "${posted_comment_id:-}" ]; then
    echo "HOMEBOY_PR_COMMENT_POSTED=true" >> "${GITHUB_ENV}"
    echo "HOMEBOY_PR_COMMENT_ID=${posted_comment_id}" >> "${GITHUB_ENV}"
  fi

  delete_comment_ids "${merge_result}"
  consolidate_canonical_comment "${section_file}"

  rm -f "${section_file}"
  echo "PR comment posted successfully"
}
