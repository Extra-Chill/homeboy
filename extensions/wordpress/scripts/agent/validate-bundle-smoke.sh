#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VALIDATOR="${SCRIPT_DIR}/validate-bundle.php"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/validate-bundle-smoke.XXXXXX")"
PASSES=0
FAILURES=0

cleanup() {
    rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

pass() {
    echo "PASS $1"
    PASSES=$((PASSES + 1))
}

fail() {
    echo "FAIL $1" >&2
    FAILURES=$((FAILURES + 1))
}

write_good_bundle() {
    rm -rf "${TMP_ROOT:?}/bundle" "${TMP_ROOT}/examples" "${TMP_ROOT}/spec.json"
    mkdir -p "${TMP_ROOT}/bundle/pipelines" "${TMP_ROOT}/bundle/flows" "${TMP_ROOT}/bundle/memory/agent" "${TMP_ROOT}/examples"

    cat > "${TMP_ROOT}/bundle/manifest.json" <<'JSON'
{
  "bundle_slug": "test-bundle",
  "agent": {
    "slug": "test-agent",
    "label": "Test Agent",
    "agent_config": {
      "daily_memory": {
        "enabled": true
      }
    }
  },
  "included": {
    "pipelines": ["test-pipeline"],
    "flows": ["test-flow"]
  },
  "run_artifacts": {
    "completion_assertions": {
      "egress": ["pr-body"]
    }
  }
}
JSON

    cat > "${TMP_ROOT}/bundle/pipelines/test-pipeline.json" <<'JSON'
{
  "slug": "test-pipeline",
  "steps": [
    {
      "step_type": "ai",
      "step_config": {
        "system_prompt": "Read source code and write a precise patch."
      }
    }
  ]
}
JSON

    cat > "${TMP_ROOT}/bundle/flows/test-flow.json" <<'JSON'
{
  "slug": "test-flow",
  "pipeline_slug": "test-pipeline",
  "steps": [
    {
      "step_type": "ai",
      "enabled_tools": ["required_tool", "agent_daily_memory"],
      "completion_assertions": []
    }
  ]
}
JSON

    cat > "${TMP_ROOT}/bundle/memory/agent/SOUL.md" <<'EOF_MEMORY'
# Test Agent
EOF_MEMORY

    cat > "${TMP_ROOT}/examples/homeboy-runner-config.example.json" <<'JSON'
{
  "success_requires_pr": false,
  "pipeline_slug": "test-pipeline"
}
JSON

    cat > "${TMP_ROOT}/spec.json" <<'JSON'
{
  "bundle_dir": "bundle",
  "bundle_slug": "test-bundle",
  "agent_slug": "test-agent",
  "agent_label": "Test Agent",
  "expected_pipelines": ["test-pipeline"],
  "expected_flows": ["test-flow"],
  "memory_files": ["SOUL.md"],
  "manifest_assertions": {
    "run_artifacts.completion_assertions.egress": ["pr-body"],
    "agent.agent_config.daily_memory.enabled": true
  },
  "flow_assertions": {
    "test-flow": {
      "pipeline_slug": "test-pipeline",
      "ai_step_required_tools": ["required_tool", "agent_daily_memory"],
      "completion_assertions_empty": true
    }
  },
  "pipeline_assertions": {
    "test-pipeline": {
      "system_prompt_must_contain": "source code"
    }
  },
  "example_runner_config": "examples/homeboy-runner-config.example.json",
  "example_assertions": {
    "success_requires_pr": false,
    "pipeline_slug": "test-pipeline"
  }
}
JSON
}

json_set() {
    local file="$1"
    local path="$2"
    local value_json="$3"

    php -r '
        $file = $argv[1];
        $path = explode(".", $argv[2]);
        $value = json_decode($argv[3], true);
        $data = json_decode((string) file_get_contents($file), true);
        $cursor =& $data;
        foreach ($path as $part) {
            $cursor =& $cursor[$part];
        }
        $cursor = $value;
        file_put_contents($file, json_encode($data, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES) . "\n");
    ' "$file" "$path" "$value_json"
}

expect_success() {
    local label="$1"
    local output

    if output="$(php "$VALIDATOR" "${TMP_ROOT}/spec.json" 2>&1)"; then
        if grep -Fq "All " <<<"$output"; then
            pass "$label"
        else
            fail "$label (success output missing summary)"
            printf '%s\n' "$output" >&2
        fi
    else
        fail "$label (validator exited non-zero)"
        printf '%s\n' "$output" >&2
    fi
}

expect_failure() {
    local label="$1"
    local expected_label="$2"
    local output

    set +e
    output="$(php "$VALIDATOR" "${TMP_ROOT}/spec.json" 2>&1)"
    local status=$?
    set -e

    if [ "$status" -eq 0 ]; then
        fail "$label (validator exited zero)"
        printf '%s\n' "$output" >&2
        return
    fi

    if grep -Fq "$expected_label" <<<"$output"; then
        pass "$label"
    else
        fail "$label (missing failure label: $expected_label)"
        printf '%s\n' "$output" >&2
    fi
}

write_good_bundle
expect_success "valid bundle passes"

write_good_bundle
json_set "${TMP_ROOT}/bundle/manifest.json" "bundle_slug" '"wrong-bundle"'
expect_failure "wrong bundle_slug fails" "manifest bundle_slug matches spec"

write_good_bundle
rm "${TMP_ROOT}/bundle/flows/test-flow.json"
expect_failure "missing flow fails" "flow file exists for test-flow"

write_good_bundle
json_set "${TMP_ROOT}/bundle/flows/test-flow.json" "pipeline_slug" '"wrong-pipeline"'
expect_failure "wrong pipeline_slug fails" "flow test-flow pipeline_slug"

write_good_bundle
json_set "${TMP_ROOT}/bundle/flows/test-flow.json" "steps.0.enabled_tools" '["agent_daily_memory"]'
expect_failure "missing required tool fails" "flow test-flow AI step 0 enables tool required_tool"

write_good_bundle
rm "${TMP_ROOT}/bundle/memory/agent/SOUL.md"
expect_failure "missing memory file fails" "memory file exists: SOUL.md"

write_good_bundle
json_set "${TMP_ROOT}/bundle/manifest.json" "agent.agent_config.daily_memory.enabled" 'false'
expect_failure "wrong manifest assertion value fails" "manifest assertion agent.agent_config.daily_memory.enabled"

if [ "$FAILURES" -gt 0 ]; then
    echo "validate-bundle smoke: ${FAILURES} failed, ${PASSES} passed" >&2
    exit 1
fi

echo "validate-bundle smoke: all ${PASSES} checks PASSED"
