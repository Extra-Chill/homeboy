#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/bench-runner-playground.sh"
TMPROOT="$(mktemp -d)"
trap 'rm -rf "$TMPROOT"' EXIT

EXTENSION_PATH="${TMPROOT}/extension"
HOST_FIXTURE_DIR="${TMPROOT}/host-plugin"
CANONICAL_DEP_DIR="${TMPROOT}/data-machine"
WORKTREE_DEP_DIR="${TMPROOT}/data-machine@fix-run-flow-paused-manual"
RUNTIME_DIR="${TMPROOT}/runtime"
BIN_DIR="${TMPROOT}/bin"
RESULTS_TMPFILE="${TMPROOT}/bench-results.json"

mkdir -p \
    "${EXTENSION_PATH}/node_modules/.bin" \
    "${HOST_FIXTURE_DIR}/tests/bench" \
    "$CANONICAL_DEP_DIR" \
    "$WORKTREE_DEP_DIR" \
    "$RUNTIME_DIR" \
    "$BIN_DIR"

cat > "${HOST_FIXTURE_DIR}/host-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Host Plugin
 * Requires Plugins: data-machine
 */
PHP

cat > "${HOST_FIXTURE_DIR}/tests/bench/noop.php" <<'PHP'
<?php
function bench_main() {
    return true;
}
PHP

cat > "${CANONICAL_DEP_DIR}/data-machine.php" <<'PHP'
<?php
/**
 * Plugin Name: Data Machine
 */
PHP

cp "${CANONICAL_DEP_DIR}/data-machine.php" "${WORKTREE_DEP_DIR}/data-machine.php"

cat > "${RUNTIME_DIR}/bench-helper.sh" <<'SH'
#!/usr/bin/env bash
homeboy_write_empty_bench_results() {
    printf '{"component":"%s","iterations":%s,"scenarios":[]}\n' "$1" "$2" > "$3"
}
SH

cat > "${RUNTIME_DIR}/bench-helper.php" <<'PHP'
<?php
PHP

cat > "${BIN_DIR}/homeboy" <<SH
#!/usr/bin/env bash
set -euo pipefail
if [ "\${1:-}" = "component" ] && [ "\${2:-}" = "show" ] && [ "\${3:-}" = "data-machine" ]; then
    printf '{"data":{"entity":{"local_path":"%s"}}}\n' "$CANONICAL_DEP_DIR"
    exit 0
fi
exit 1
SH
chmod +x "${BIN_DIR}/homeboy"

cat > "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

wrapper=""
canonical_mounts=0
suffix_mounts=0
worktree_canonical_mounts=0

while [ "$#" -gt 0 ]; do
    if [ "$1" = "--mount" ]; then
        mount_arg="$2"
        case "$mount_arg" in
            *:/runner.php)
                wrapper="${mount_arg%:/runner.php}"
                ;;
            *:/wordpress/wp-content/plugins/data-machine)
                canonical_mounts=$((canonical_mounts + 1))
                case "$mount_arg" in
                    *data-machine@fix-run-flow-paused-manual:/wordpress/wp-content/plugins/data-machine)
                        worktree_canonical_mounts=$((worktree_canonical_mounts + 1))
                        ;;
                esac
                ;;
            *:/wordpress/wp-content/plugins/data-machine@fix-run-flow-paused-manual)
                suffix_mounts=$((suffix_mounts + 1))
                ;;
        esac
        shift 2
        continue
    fi
    shift
done

if [ -z "$wrapper" ] || [ ! -f "$wrapper" ]; then
    echo "runner wrapper mount not found" >&2
    exit 1
fi

if [ "$canonical_mounts" -ne 1 ]; then
    echo "expected exactly one canonical data-machine dependency mount, got $canonical_mounts" >&2
    exit 1
fi

if [ "$worktree_canonical_mounts" -ne 1 ]; then
    echo "expected the worktree dependency to mount at canonical slug data-machine" >&2
    exit 1
fi

if [ "$suffix_mounts" -ne 0 ]; then
    echo "worktree-suffixed dependency was mounted as a second plugin copy" >&2
    exit 1
fi

if grep -Fq '/wordpress/wp-content/plugins/data-machine@fix-run-flow-paused-manual' "$wrapper"; then
    echo "runner wrapper would load the worktree-suffixed dependency slug" >&2
    exit 1
fi

if ! grep -Fq '/wordpress/wp-content/plugins/data-machine' "$wrapper"; then
    echo "runner wrapper missing canonical dependency load path" >&2
    exit 1
fi

cat > "${HOMEBOY_PLUGIN_PATH}/.pg-bench-results.json" <<'JSON'
{"component":"host-plugin","iterations":1,"scenarios":[]}
JSON
SH
chmod +x "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli"

SETTINGS_JSON=$(jq -n --arg dep "$WORKTREE_DEP_DIR" '{"validation_dependencies": [$dep]}')

PATH="${BIN_DIR}:${PATH}" \
HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_COMPONENT_ID=host-plugin \
HOMEBOY_COMPONENT_PATH="$HOST_FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_RUNTIME_BENCH_HELPER_SH="${RUNTIME_DIR}/bench-helper.sh" \
HOMEBOY_RUNTIME_BENCH_HELPER_PHP="${RUNTIME_DIR}/bench-helper.php" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    bash "$RUNNER"

if [ ! -s "$RESULTS_TMPFILE" ]; then
    echo "expected bench results file to be written" >&2
    exit 1
fi

echo "Playground bench dependency worktree dedupe smoke passed"
