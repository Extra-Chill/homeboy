#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER="${SCRIPT_DIR}/../scripts/lib/validation-dependencies.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

COMPONENT_DIR="${TMPDIR}/intelligence"
DATA_MACHINE_DIR="${TMPDIR}/data-machine"
AGENTS_API_DIR="${TMPDIR}/agents-api"
BIN_DIR="${TMPDIR}/bin"

mkdir -p "$COMPONENT_DIR" "$DATA_MACHINE_DIR" "$AGENTS_API_DIR" "$BIN_DIR"

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

cat > "${AGENTS_API_DIR}/agents-api.php" <<'PHP'
<?php
/**
 * Plugin Name: Agents API
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

source "$HELPER"

resolved=$(PATH="${BIN_DIR}:$PATH" homeboy_resolve_validation_dependency_paths "$COMPONENT_DIR")

if ! grep -F -- "$DATA_MACHINE_DIR" <<< "$resolved" >/dev/null; then
    echo "FAIL: direct Requires Plugins dependency was not resolved" >&2
    printf '%s\n' "$resolved" >&2
    exit 1
fi

if ! grep -F -- "$AGENTS_API_DIR" <<< "$resolved" >/dev/null; then
    echo "FAIL: transitive Requires Plugins dependency was not resolved" >&2
    printf '%s\n' "$resolved" >&2
    exit 1
fi

echo "Validation dependency smoke passed"
