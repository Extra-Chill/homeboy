#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER="${SCRIPT_DIR}/../scripts/lib/validation-dependencies.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

COMPONENT_DIR="${TMPDIR}/intelligence"
DATA_MACHINE_DIR="${TMPDIR}/data-machine"
CLONED_DATA_MACHINE_DIR="${TMPDIR}/cache/data-machine"
AGENTS_API_DIR="${TMPDIR}/agents-api"
VENDORED_AGENTS_API_DIR="${COMPONENT_DIR}/vendor/automattic/agents-api"
OPENAI_PROVIDER_DIR="${TMPDIR}/ai-provider-for-openai"
BIN_DIR="${TMPDIR}/bin"
CLONE_BIN_DIR="${TMPDIR}/clone-bin"
FALLBACK_CACHE_DIR="${TMPDIR}/fallback-cache"

mkdir -p "$COMPONENT_DIR" "$DATA_MACHINE_DIR" "$CLONED_DATA_MACHINE_DIR" "$AGENTS_API_DIR" "$VENDORED_AGENTS_API_DIR" "$OPENAI_PROVIDER_DIR" "$BIN_DIR" "$CLONE_BIN_DIR" "$FALLBACK_CACHE_DIR"

cat > "${COMPONENT_DIR}/intelligence.php" <<'PHP'
<?php
/**
 * Plugin Name: Intelligence
 * Requires Plugins: data-machine
 */
PHP

cat > "${DATA_MACHINE_DIR}/data-machine.php" <<'PHP'
<?php
/**
 * Plugin Name: Data Machine
 * Requires Plugins: agents-api
 */
PHP

cat > "${CLONED_DATA_MACHINE_DIR}/data-machine.php" <<'PHP'
<?php
/**
 * Plugin Name: Data Machine
 */
PHP

cat > "${AGENTS_API_DIR}/agents-api.php" <<'PHP'
<?php
/**
 * Plugin Name: Agents API
 */
PHP

cat > "${VENDORED_AGENTS_API_DIR}/agents-api.php" <<'PHP'
<?php
/**
 * Plugin Name: Agents API Locked Vendor Copy
 */
PHP

cat > "${COMPONENT_DIR}/composer.lock" <<'JSON'
{
    "packages": [
        {
            "name": "automattic/agents-api"
        }
    ]
}
JSON

cat > "${OPENAI_PROVIDER_DIR}/ai-provider-for-openai.php" <<'PHP'
<?php
/**
 * Plugin Name: AI Provider for OpenAI
 */
PHP

cat > "${BIN_DIR}/homeboy" <<SH
#!/usr/bin/env bash
set -euo pipefail
if [ "\${1:-}" = "component" ] && [ "\${2:-}" = "show" ]; then
    case "\${3:-}" in
        data-machine)
            printf '{"data":{"entity":{"local_path":"%s"}}}\n' '${DATA_MACHINE_DIR}'
            ;;
        agents-api)
            printf '{"data":{"entity":{"local_path":"%s"}}}\n' '${AGENTS_API_DIR}'
            ;;
        *)
            printf '{"data":{"entity":{}}}\n'
            ;;
    esac
fi
SH
chmod +x "${BIN_DIR}/homeboy"

cat > "${CLONE_BIN_DIR}/homeboy" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "component" ] && [ "${2:-}" = "show" ]; then
    printf '%s\n' '{"data":{"entity":{}}}'
fi
SH
chmod +x "${CLONE_BIN_DIR}/homeboy"

cat > "${CLONE_BIN_DIR}/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [ "${1:-}" = "-C" ]; then
    shift 2
fi

if [ "${1:-}" = "remote" ] && [ "${2:-}" = "get-url" ] && [ "${3:-}" = "origin" ]; then
    printf '%s\n' 'https://github.com/Automattic/intelligence.git'
    exit 0
fi

if [ "${1:-}" = "clone" ]; then
    repo_url="${@: -2:1}"
    clone_path="${@: -1}"
    case "$repo_url" in
        *github.com/Automattic/data-machine.git)
            echo "FAIL: data-machine should resolve through its canonical Extra-Chill owner" >&2
            exit 1
            ;;
        *github.com/Extra-Chill/data-machine.git)
            mkdir -p "$clone_path"
            cat > "${clone_path}/data-machine.php" <<'PHP'
<?php
/**
 * Plugin Name: Data Machine
 */
PHP
            exit 0
            ;;
    esac
fi

exit 1
SH
chmod +x "${CLONE_BIN_DIR}/git"

source "$HELPER"

resolved=$(PATH="${BIN_DIR}:$PATH" homeboy_resolve_validation_dependency_paths "$COMPONENT_DIR")

if ! grep -F -- "$DATA_MACHINE_DIR" <<< "$resolved" >/dev/null; then
    echo "FAIL: direct Requires Plugins dependency was not resolved" >&2
    printf '%s\n' "$resolved" >&2
    exit 1
fi

if ! grep -F -- "$VENDORED_AGENTS_API_DIR" <<< "$resolved" >/dev/null; then
    echo "FAIL: transitive Requires Plugins dependency should prefer composer-locked vendor copy" >&2
    printf '%s\n' "$resolved" >&2
    exit 1
fi

if grep -F -- "$AGENTS_API_DIR" <<< "$resolved" >/dev/null; then
    echo "FAIL: stale local registry checkout should not override composer-locked vendor copy" >&2
    printf '%s\n' "$resolved" >&2
    exit 1
fi

agents_api_line=$(grep -nF -- "$VENDORED_AGENTS_API_DIR" <<< "$resolved" | cut -d: -f1 | head -1)
data_machine_line=$(grep -nF -- "$DATA_MACHINE_DIR" <<< "$resolved" | cut -d: -f1 | head -1)
if [ "$agents_api_line" -ge "$data_machine_line" ]; then
    echo "FAIL: transitive dependency should be emitted before dependent plugin" >&2
    printf '%s\n' "$resolved" >&2
    exit 1
fi

fallback_resolved=$(
    PATH="${CLONE_BIN_DIR}:$PATH" \
    HOMEBOY_CACHE_DIR="$FALLBACK_CACHE_DIR" \
    homeboy_resolve_validation_dependency_paths "$COMPONENT_DIR"
)
fallback_data_machine="${FALLBACK_CACHE_DIR}/homeboy-deps/data-machine"
if ! grep -F -- "$fallback_data_machine" <<< "$fallback_resolved" >/dev/null; then
    echo "FAIL: data-machine dependency should resolve from Extra-Chill/data-machine" >&2
    printf '%s\n' "$fallback_resolved" >&2
    exit 1
fi

merged=$(homeboy_merge_validation_dependency_paths "$DATA_MACHINE_DIR" "$CLONED_DATA_MACHINE_DIR")
if ! grep -F -- "$DATA_MACHINE_DIR" <<< "$merged" >/dev/null; then
    echo "FAIL: existing prepared dependency path was not preserved" >&2
    printf '%s\n' "$merged" >&2
    exit 1
fi

if grep -F -- "$CLONED_DATA_MACHINE_DIR" <<< "$merged" >/dev/null; then
    echo "FAIL: cloned dependency path was not deduplicated by plugin slug" >&2
    printf '%s\n' "$merged" >&2
    exit 1
fi

prepared_paths="${DATA_MACHINE_DIR}"$'\n'"${OPENAI_PROVIDER_DIR}"
merged_prepared=$(homeboy_merge_validation_dependency_paths "$prepared_paths" "$CLONED_DATA_MACHINE_DIR")
if ! grep -F -- "$DATA_MACHINE_DIR" <<< "$merged_prepared" >/dev/null || ! grep -F -- "$OPENAI_PROVIDER_DIR" <<< "$merged_prepared" >/dev/null; then
    echo "FAIL: multi-line prepared dependency paths were not preserved" >&2
    printf '%s\n' "$merged_prepared" >&2
    exit 1
fi

if grep -F -- "$CLONED_DATA_MACHINE_DIR" <<< "$merged_prepared" >/dev/null; then
    echo "FAIL: multi-line prepared dependency paths did not deduplicate cloned dependency" >&2
    printf '%s\n' "$merged_prepared" >&2
    exit 1
fi

topological_resolved="${VENDORED_AGENTS_API_DIR}"$'\n'"${DATA_MACHINE_DIR}"
merged_topological=$(homeboy_merge_validation_dependency_paths "$prepared_paths" "$topological_resolved")
agents_api_merged_line=$(grep -nF -- "$VENDORED_AGENTS_API_DIR" <<< "$merged_topological" | cut -d: -f1 | head -1)
data_machine_merged_line=$(grep -nF -- "$DATA_MACHINE_DIR" <<< "$merged_topological" | cut -d: -f1 | head -1)
if [ "$agents_api_merged_line" -ge "$data_machine_merged_line" ]; then
    echo "FAIL: merge should preserve resolved transitive dependency order" >&2
    printf '%s\n' "$merged_topological" >&2
    exit 1
fi

if ! grep -F -- "$OPENAI_PROVIDER_DIR" <<< "$merged_topological" >/dev/null; then
    echo "FAIL: merge should append unrelated prepared dependency paths" >&2
    printf '%s\n' "$merged_topological" >&2
    exit 1
fi

export_log=$(mktemp)
HOMEBOY_WORDPRESS_DEPENDENCY_PATHS="$AGENTS_API_DIR" \
HOMEBOY_SETTINGS_JSON='{"validation_dependencies":"agents-api"}' \
PATH="${BIN_DIR}:$PATH" \
homeboy_export_validation_dependency_paths "$COMPONENT_DIR" 2>"$export_log"
export_output=$(cat "$export_log")
if ! grep -F -- "Resolved dependency 'agents-api' via final validation dependency path: ${AGENTS_API_DIR}" <<< "$export_output" >/dev/null; then
    echo "FAIL: exported dependency diagnostics should report the final selected path" >&2
    printf '%s\n' "$export_output" >&2
    exit 1
fi

if grep -F -- "Homeboy component registry" <<< "$export_output" >/dev/null; then
    echo "FAIL: exported dependency diagnostics should not report discarded resolver candidates" >&2
    printf '%s\n' "$export_output" >&2
    exit 1
fi

echo "Validation dependency smoke passed"
