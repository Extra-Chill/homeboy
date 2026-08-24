#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../lib/wp-codebox-paths.sh"

usage() {
    cat >&2 <<'USAGE'
Usage: validate-playground-blueprint.sh <blueprint-file-or-url> [--wp VERSION] [--php VERSION] [--artifact-dir DIR]

Runs the supplied Blueprint through wp-codebox validate-blueprint and prints
captured stdout/stderr on failure so CI catches public Blueprint boot errors.

Set HOMEBOY_WP_CODEBOX_BIN to use a specific wp-codebox binary.
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
PHP_VERSION=""
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
homeboy_wp_codebox_export_command "${HOMEBOY_SETTINGS_JSON:-}"
homeboy_wp_codebox_preflight_command
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
if [ -n "$PHP_VERSION" ]; then
    echo "  PHP: $PHP_VERSION (wp-codebox default runtime; --php is accepted for CLI compatibility)"
fi
if [ -n "$ARTIFACT_DIR" ]; then
    echo "  Artifacts: $ARTIFACT_DIR"
fi

WP_CODEBOX_ARGS=(validate-blueprint --blueprint "$BLUEPRINT" --wp "$WP_VERSION")
if [ -n "$ARTIFACT_DIR" ]; then
    WP_CODEBOX_ARGS+=(--artifacts "$ARTIFACT_DIR")
fi

set +e
"${HOMEBOY_WP_CODEBOX_COMMAND[@]}" "${WP_CODEBOX_ARGS[@]}" >"$OUTPUT_FILE" 2>&1
status=$?
set -e

if [ $status -ne 0 ]; then
    if [ -n "$ARTIFACT_DIR" ]; then
        cp "$OUTPUT_FILE" "$ARTIFACT_DIR/playground-blueprint.log"
        {
            echo "Blueprint: $BLUEPRINT"
            echo "WordPress: $WP_VERSION"
            if [ -n "$PHP_VERSION" ]; then
                echo "PHP: $PHP_VERSION (requested; wp-codebox owns the runtime PHP version)"
            fi
            echo "Runtime: wp-codebox"
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
    echo "--- wp-codebox validate-blueprint output ---" >&2
    cat "$OUTPUT_FILE" >&2
    if [ -n "$ARTIFACT_DIR" ]; then
        echo "" >&2
        echo "Artifacts written to: $ARTIFACT_DIR" >&2
    fi
    exit $status
fi

echo "Playground Blueprint validation passed"
