#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
PLAYGROUND_CLI="${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli"

usage() {
    cat >&2 <<'USAGE'
Usage: validate-playground-blueprint.sh <blueprint-file-or-url> [--wp VERSION] [--php VERSION] [--artifact-dir DIR]

Runs the supplied Blueprint through wp-playground-cli run-blueprint and prints
captured stdout/stderr on failure so CI catches public Blueprint boot errors.
USAGE
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    usage
    exit 0
fi

if [ $# -lt 1 ]; then
    usage
    exit 2
fi

BLUEPRINT="$1"
shift

WP_VERSION="latest"
PHP_VERSION="8.3"
ARTIFACT_DIR="${HOMEBOY_ARTIFACT_DIR:-}"

while [ $# -gt 0 ]; do
    case "$1" in
        --wp)
            if [ $# -lt 2 ]; then
                echo "Error: --wp requires a value" >&2
                exit 2
            fi
            WP_VERSION="${2:-}"
            shift 2
            ;;
        --php)
            if [ $# -lt 2 ]; then
                echo "Error: --php requires a value" >&2
                exit 2
            fi
            PHP_VERSION="${2:-}"
            shift 2
            ;;
        --artifact-dir)
            if [ $# -lt 2 ]; then
                echo "Error: --artifact-dir requires a value" >&2
                exit 2
            fi
            ARTIFACT_DIR="${2:-}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Error: unknown argument: $1" >&2
            usage
            exit 2
            ;;
    esac
done

if [ -z "$BLUEPRINT" ]; then
    echo "Error: blueprint path or URL is required" >&2
    exit 2
fi
if [ ! -f "$PLAYGROUND_CLI" ]; then
    echo "Error: @wp-playground/cli not found at $PLAYGROUND_CLI" >&2
    echo "Install it with: cd ${EXTENSION_PATH} && npm install" >&2
    exit 2
fi
if [ -n "$ARTIFACT_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR"
fi

OUTPUT_FILE=$(mktemp "${TMPDIR:-/tmp}/playground-blueprint.XXXXXX.log")
cleanup() {
    rm -f "$OUTPUT_FILE"
}
trap cleanup EXIT

echo "Validating WordPress Playground Blueprint..."
echo "  Blueprint: $BLUEPRINT"
echo "  WordPress: $WP_VERSION"
echo "  PHP: $PHP_VERSION"
if [ -n "$ARTIFACT_DIR" ]; then
    echo "  Artifacts: $ARTIFACT_DIR"
fi

set +e
"$PLAYGROUND_CLI" run-blueprint \
    --blueprint "$BLUEPRINT" \
    --wp "$WP_VERSION" \
    --php "$PHP_VERSION" \
    --verbosity=debug \
    >"$OUTPUT_FILE" 2>&1
status=$?
set -e

if [ $status -ne 0 ]; then
    if [ -n "$ARTIFACT_DIR" ]; then
        cp "$OUTPUT_FILE" "$ARTIFACT_DIR/playground-blueprint.log"
        {
            echo "Blueprint: $BLUEPRINT"
            echo "WordPress: $WP_VERSION"
            echo "PHP: $PHP_VERSION"
            echo "Exit code: $status"
            echo "Generated: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
        } >"$ARTIFACT_DIR/playground-blueprint-summary.txt"
    fi

    echo ""
    echo "============================================" >&2
    echo "PLAYGROUND BLUEPRINT VALIDATION FAILED" >&2
    echo "============================================" >&2
    echo "Blueprint: $BLUEPRINT" >&2
    echo "Exit code: $status" >&2
    echo "" >&2
    echo "--- wp-playground-cli output ---" >&2
    cat "$OUTPUT_FILE" >&2
    echo "" >&2
    echo "--- recent Playground temp sites ---" >&2
    ls -td "${TMPDIR:-/tmp}"/node-playground-cli-site-* 2>/dev/null | head -5 >&2 || true
    if [ -n "$ARTIFACT_DIR" ]; then
        echo "" >&2
        echo "Artifacts written to: $ARTIFACT_DIR" >&2
    fi
    exit $status
fi

echo "Playground Blueprint validation passed"
