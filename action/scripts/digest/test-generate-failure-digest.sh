#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

BIN_DIR="${TMPDIR}/bin"
OUTPUT_DIR="${TMPDIR}/output"
SUMMARY_FILE="${TMPDIR}/summary.md"
ENV_FILE="${TMPDIR}/github-env"
ARGS_FILE="${TMPDIR}/homeboy-args"
mkdir -p "${BIN_DIR}" "${OUTPUT_DIR}"

cat > "${BIN_DIR}/homeboy" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$@" > "${HOMEBOY_STUB_ARGS_FILE}"
cat <<'MARKDOWN'
## Failure Digest

### Test Failure Digest
- Failed tests: **1**
MARKDOWN
STUB
chmod +x "${BIN_DIR}/homeboy"

export PATH="${BIN_DIR}:${PATH}"
export HOMEBOY_STUB_ARGS_FILE="${ARGS_FILE}"
export HOMEBOY_OUTPUT_DIR="${OUTPUT_DIR}"
export RESULTS='{"review lint":"fail","review test":"fail"}'
export GITHUB_SERVER_URL="https://github.com"
export GITHUB_REPOSITORY="Extra-Chill/homeboy-action"
export GITHUB_RUN_ID="12345"
export HOMEBOY_CLI_VERSION="homeboy 1.2.3"
export HOMEBOY_EXTENSION_ID="nodejs"
export HOMEBOY_EXTENSION_SOURCE="https://github.com/Extra-Chill/homeboy-extensions"
export HOMEBOY_EXTENSION_REVISION="abc123"
export HOMEBOY_ACTION_REPOSITORY="Extra-Chill/homeboy-action"
export HOMEBOY_ACTION_REF="v2"
export COMMANDS="review lint,review test,review audit"
export COMPONENT_NAME="wordpress"
export SCOPE_CONTEXT="pr"
export SCOPE_MODE="changed"
export SCOPE_BASE_REF="abc123base"
export GITHUB_ENV="${ENV_FILE}"
export GITHUB_STEP_SUMMARY="${SUMMARY_FILE}"

bash "${ROOT}/scripts/digest/generate-failure-digest.sh"

DIGEST_FILE="${OUTPUT_DIR}/failure-digest.md"
TOOLING_JSON="${OUTPUT_DIR}/failure-digest-tooling.json"

if [ ! -f "${DIGEST_FILE}" ]; then
  echo "missing digest file" >&2
  exit 1
fi

grep -F "HOMEBOY_FAILURE_DIGEST_FILE=${DIGEST_FILE}" "${ENV_FILE}" >/dev/null
grep -F "### Test Failure Digest" "${SUMMARY_FILE}" >/dev/null
grep -F "### Local reproduction" "${DIGEST_FILE}" >/dev/null
grep -F -- "- Scope context: \`pr\`" "${DIGEST_FILE}" >/dev/null
grep -F -- "- Scope mode: \`changed\`" "${DIGEST_FILE}" >/dev/null
grep -F -- "- Scope base ref: \`abc123base\`" "${DIGEST_FILE}" >/dev/null
grep -F "homeboy review lint wordpress --path . --changed-since abc123base" "${DIGEST_FILE}" >/dev/null
grep -F "homeboy review test wordpress --path . --changed-since abc123base" "${DIGEST_FILE}" >/dev/null

grep -Fx -- "report" "${ARGS_FILE}" >/dev/null
grep -Fx -- "failure-digest" "${ARGS_FILE}" >/dev/null
grep -Fx -- "--output-dir" "${ARGS_FILE}" >/dev/null
grep -Fx -- "${OUTPUT_DIR}" "${ARGS_FILE}" >/dev/null
grep -Fx -- "--results" "${ARGS_FILE}" >/dev/null
grep -Fx -- "${RESULTS}" "${ARGS_FILE}" >/dev/null
grep -Fx -- "--run-url" "${ARGS_FILE}" >/dev/null
grep -Fx -- "https://github.com/Extra-Chill/homeboy-action/actions/runs/12345" "${ARGS_FILE}" >/dev/null
grep -Fx -- "--tooling-json" "${ARGS_FILE}" >/dev/null
grep -Fx -- "${TOOLING_JSON}" "${ARGS_FILE}" >/dev/null
grep -Fx -- "--commands" "${ARGS_FILE}" >/dev/null
grep -Fx -- "review lint,review test,review audit" "${ARGS_FILE}" >/dev/null

jq -e \
  '.homeboy_cli_version == "homeboy 1.2.3" and
   .extension_id == "nodejs" and
   .extension_source == "https://github.com/Extra-Chill/homeboy-extensions" and
   .extension_revision == "abc123" and
   .action_repository == "Extra-Chill/homeboy-action" and
   .action_ref == "v2"' \
  "${TOOLING_JSON}" >/dev/null

export RESULTS='{"review test":"baseline_red"}'
cat > "${OUTPUT_DIR}/baseline-status.json" <<'JSON'
{
  "review test": {
    "status": "fail",
    "exit_code": 1,
    "command": "homeboy review test wordpress --path /work --changed-since abc123base",
    "structured_output": false
  }
}
JSON

bash "${ROOT}/scripts/digest/generate-failure-digest.sh"

grep -F "### Differential baseline evidence" "${DIGEST_FILE}" >/dev/null
grep -F -- '- `review test`: **baseline_red**' "${DIGEST_FILE}" >/dev/null
grep -F -- '- Baseline command: `homeboy review test wordpress --path /work --changed-since abc123base`' "${DIGEST_FILE}" >/dev/null
grep -F -- '- Baseline result: `fail` (exit `1`)' "${DIGEST_FILE}" >/dev/null
grep -F -- '- Candidate result: `baseline_red`' "${DIGEST_FILE}" >/dev/null
grep -F -- '- Artifact refs: `review-test.json`, `baseline-review-test.json`, `baseline-review-test.log`' "${DIGEST_FILE}" >/dev/null

echo "generate failure digest wrapper checks passed"
