#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
POLICY_GATE="${SCRIPT_DIR}/policy-gate.sh"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

FAKE_BIN="${TMPDIR}/bin"
mkdir -p "${FAKE_BIN}"

cat > "${FAKE_BIN}/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [ "$1" = "api" ]; then
  printf '%s\n' "${GH_FAKE_FILES}"
  exit 0
fi

if [ "$1" = "pr" ] && [ "$2" = "merge" ]; then
  printf '%s\n' "$*" >> "${GH_FAKE_MERGES}"
  exit 0
fi

printf 'unexpected gh invocation: %s\n' "$*" >&2
exit 1
EOF
chmod +x "${FAKE_BIN}/gh"

assert_policy() {
  local expected_safe="$1"
  local expected_merged="$2"
  local label="$3"
  shift 3

  local output_file merge_file output safe merged
  output_file="${TMPDIR}/output-${label// /-}"
  merge_file="${TMPDIR}/merge-${label// /-}"
  : > "${merge_file}"

  PATH="${FAKE_BIN}:${PATH}" \
    GITHUB_OUTPUT="${output_file}" \
    GH_FAKE_MERGES="${merge_file}" \
    "$@" >/dev/null

  safe="$(awk '/^safe<<HOMEBOY_PR_POLICY$/{getline; print; exit}' "${output_file}")"
  merged="$(awk '/^merged<<HOMEBOY_PR_POLICY$/{getline; print; exit}' "${output_file}")"

  if [ "${safe}" != "${expected_safe}" ] || [ "${merged}" != "${expected_merged}" ]; then
    printf 'FAIL: %s\nexpected safe/merged: %s/%s\nactual safe/merged:   %s/%s\n' \
      "${label}" "${expected_safe}" "${expected_merged}" "${safe}" "${merged}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

POLICY="${TMPDIR}/policy.json"
cat > "${POLICY}" <<'EOF'
{
  "allowed_authors": ["github-actions[bot]"],
  "allowed_head_branches": ["world-day/**"],
  "allowed_paths": ["content/**", "themes/world-of-wordpress/patterns/**"],
  "blocked_paths": [".github/**", "bundles/**"],
  "blocked_content_patterns": ["eval[[:space:]]*\\("]
}
EOF

mkdir -p "${TMPDIR}/repo/content/page" "${TMPDIR}/repo/.github/workflows"
printf '<!-- wp:paragraph --><p>Hello</p><!-- /wp:paragraph -->\n' > "${TMPDIR}/repo/content/page/world-index.md"
printf 'eval( $code );\n' > "${TMPDIR}/repo/content/page/bad.md"

assert_policy true false "safe content" \
  env -C "${TMPDIR}/repo" \
    POLICY_PATH="${POLICY}" \
    PR_NUMBER="7" \
    REPOSITORY="chubes4/world-of-wordpress" \
    PR_AUTHOR="github-actions[bot]" \
    PR_HEAD_REF="world-day/2026-05-10" \
    PR_HEAD_REPO="chubes4/world-of-wordpress" \
    GH_FAKE_FILES='content/page/world-index.md' \
    bash "${POLICY_GATE}"

assert_policy false false "blocked path" \
  env -C "${TMPDIR}/repo" \
    POLICY_PATH="${POLICY}" \
    PR_NUMBER="7" \
    REPOSITORY="chubes4/world-of-wordpress" \
    PR_AUTHOR="github-actions[bot]" \
    PR_HEAD_REF="world-day/2026-05-10" \
    PR_HEAD_REPO="chubes4/world-of-wordpress" \
    GH_FAKE_FILES='.github/workflows/world.yml' \
    bash "${POLICY_GATE}"

assert_policy false false "blocked content" \
  env -C "${TMPDIR}/repo" \
    POLICY_PATH="${POLICY}" \
    PR_NUMBER="7" \
    REPOSITORY="chubes4/world-of-wordpress" \
    PR_AUTHOR="github-actions[bot]" \
    PR_HEAD_REF="world-day/2026-05-10" \
    PR_HEAD_REPO="chubes4/world-of-wordpress" \
    GH_FAKE_FILES='content/page/bad.md' \
    bash "${POLICY_GATE}"

assert_policy true true "safe merge" \
  env -C "${TMPDIR}/repo" \
    POLICY_PATH="${POLICY}" \
    POLICY_MERGE="true" \
    POLICY_MERGE_METHOD="squash" \
    PR_NUMBER="7" \
    REPOSITORY="chubes4/world-of-wordpress" \
    PR_AUTHOR="github-actions[bot]" \
    PR_HEAD_REF="world-day/2026-05-10" \
    PR_HEAD_REPO="chubes4/world-of-wordpress" \
    GH_FAKE_FILES='content/page/world-index.md' \
    bash "${POLICY_GATE}"

printf 'All PR policy gate checks passed.\n'
