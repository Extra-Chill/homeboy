#!/usr/bin/env bash
#
# Emit an attribution report when a post-merge gate finds `main` red.
#
# The whole value of the post-merge gate is knowing WHICH merge broke main, so
# this prints the merge commit, its author, and the compare range for the push.
#
# All untrusted values (commit subject, author name) arrive via the environment
# and are only ever expanded inside quotes — never interpolated into the script
# body by the workflow. A commit message is attacker-influenced input.

set -euo pipefail

: "${MERGE_SHA:?MERGE_SHA is required}"
: "${GATE:?GATE is required}"

merge_before="${MERGE_BEFORE:-}"
merge_author="${MERGE_AUTHOR:-unknown}"

# First line only — commit bodies can be arbitrarily long.
subject="$(printf '%s\n' "${MERGE_SUBJECT:-(no subject)}" | head -n 1)"

short_sha="$(printf '%s' "${MERGE_SHA}" | cut -c1-9)"
short_before="$(printf '%s' "${merge_before}" | cut -c1-9)"
repo_url="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-}"

{
  echo "## \`main\` is red after this merge"
  echo
  echo "The **${GATE}** gate failed on the merged result. This is the full"
  echo "release-blocking suite, so the next release would fail at"
  echo "\`Release Quality Policy\` → \`${GATE}\` unless this is fixed first."
  echo
  echo "| | |"
  echo "| --- | --- |"
  echo "| Merge commit | [\`${short_sha}\`](${repo_url}/commit/${MERGE_SHA}) |"
  echo "| Author | ${merge_author} |"
  echo "| Subject | ${subject} |"
} >>"${GITHUB_STEP_SUMMARY:-/dev/stdout}"

# The all-zero SHA is what GitHub sends for a branch's first push.
if [ -n "${merge_before}" ] && [ "${merge_before}" != "0000000000000000000000000000000000000000" ]; then
  {
    echo "| Pushed range | [\`${short_before}...${short_sha}\`](${repo_url}/compare/${merge_before}...${MERGE_SHA}) |"
    echo
    echo "> Runs are cancelled by newer merges (\`cancel-in-progress: true\`), so if"
    echo "> a burst landed together the culprit may be any merge since the last"
    echo "> green run of this workflow — not necessarily the one named above."
    echo "> Widen the compare range to that run's commit to see the full candidate set."
  } >>"${GITHUB_STEP_SUMMARY:-/dev/stdout}"
fi

echo "::error::main is red after ${short_sha} (${GATE} gate): ${subject}"
