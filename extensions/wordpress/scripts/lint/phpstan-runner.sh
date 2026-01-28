#!/usr/bin/env bash
set -euo pipefail

# Standalone PHP static analysis script using PHPStan
# Supports summary mode via HOMEBOY_SUMMARY_MODE=1
# Supports skip via HOMEBOY_SKIP_PHPSTAN=1
# Supports level override via HOMEBOY_PHPSTAN_LEVEL (default: 5)

# Debug environment variables (only shown when HOMEBOY_DEBUG=1)
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: PHPStan Environment variables:"
    echo "HOMEBOY_MODULE_PATH=${HOMEBOY_MODULE_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_ID=${HOMEBOY_COMPONENT_ID:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_SUMMARY_MODE=${HOMEBOY_SUMMARY_MODE:-NOT_SET}"
    echo "HOMEBOY_SKIP_PHPSTAN=${HOMEBOY_SKIP_PHPSTAN:-NOT_SET}"
    echo "HOMEBOY_PHPSTAN_LEVEL=${HOMEBOY_PHPSTAN_LEVEL:-NOT_SET}"
fi

# Skip if explicitly requested
if [[ "${HOMEBOY_SKIP_PHPSTAN:-}" == "1" ]]; then
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Skipping PHPStan (HOMEBOY_SKIP_PHPSTAN=1)"
    fi
    exit 0
fi

# Determine execution context
if [ -n "${HOMEBOY_MODULE_PATH:-}" ]; then
    MODULE_PATH="${HOMEBOY_MODULE_PATH}"
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:-.}"
    PLUGIN_PATH="$COMPONENT_PATH"
else
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    MODULE_PATH="$(dirname "$(dirname "$SCRIPT_DIR")")"
    COMPONENT_PATH="$(pwd)"
    PLUGIN_PATH="$COMPONENT_PATH"
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Module path: $MODULE_PATH"
    echo "DEBUG: Plugin path: $PLUGIN_PATH"
fi

PHPSTAN_BIN="${MODULE_PATH}/vendor/bin/phpstan"
PHPSTAN_CONFIG="${MODULE_PATH}/phpstan.neon.dist"
COMPONENT_BASELINE="${PLUGIN_PATH}/phpstan-baseline.neon"

# Validate PHPStan exists (soft failure - not all installations have it)
if [ ! -f "$PHPSTAN_BIN" ]; then
    echo "Warning: PHPStan not found at $PHPSTAN_BIN, skipping static analysis"
    exit 0
fi

if [ ! -f "$PHPSTAN_CONFIG" ]; then
    echo "Warning: phpstan.neon.dist not found at $PHPSTAN_CONFIG, skipping static analysis"
    exit 0
fi

# Check if component has PHP files
php_file_count=$(find "$PLUGIN_PATH" -type f -name "*.php" \
    -not -path "*/vendor/*" \
    -not -path "*/node_modules/*" \
    -not -path "*/build/*" \
    -not -path "*/dist/*" \
    -not -path "*/tests/*" \
    2>/dev/null | wc -l | tr -d ' ')

if [ "$php_file_count" -eq 0 ]; then
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: No PHP files found, skipping PHPStan"
    fi
    exit 0
fi

echo "Running PHPStan static analysis..."

# Build PHPStan arguments
phpstan_args=(analyse)
phpstan_args+=(--configuration="$PHPSTAN_CONFIG")

# Level override (default: 5)
PHPSTAN_LEVEL="${HOMEBOY_PHPSTAN_LEVEL:-5}"
phpstan_args+=(--level="$PHPSTAN_LEVEL")

# Include component baseline if it exists
if [ -f "$COMPONENT_BASELINE" ]; then
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Using component baseline: $COMPONENT_BASELINE"
    fi
    phpstan_args+=(--baseline="$COMPONENT_BASELINE")
fi

# No progress bar for cleaner output
phpstan_args+=(--no-progress)

# Add the path to analyze
phpstan_args+=("$PLUGIN_PATH")

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: PHPStan command: $PHPSTAN_BIN ${phpstan_args[*]}"
fi

# Summary mode: get JSON output and parse it
if [[ "${HOMEBOY_SUMMARY_MODE:-}" == "1" ]]; then
    set +e
    json_output=$("$PHPSTAN_BIN" "${phpstan_args[@]}" --error-format=json 2>/dev/null)
    json_exit=$?
    set -e

    # Parse JSON and print summary
    if [ -n "$json_output" ] && command -v php &> /dev/null; then
        summary=$(echo "$json_output" | php -r '
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json) exit;

            $totals = $json["totals"] ?? [];
            $errors = $totals["file_errors"] ?? 0;
            $files = count($json["files"] ?? []);

            if ($errors > 0) {
                echo "============================================\n";
                echo "PHPSTAN SUMMARY: " . $errors . " errors at level " . ($argv[1] ?? "5") . "\n";
                echo "Files with issues: " . $files . "\n";
                echo "============================================\n";
            }
        ' "$PHPSTAN_LEVEL" 2>/dev/null)

        if [ -n "$summary" ]; then
            echo ""
            echo "$summary"
        fi

        # Show top error types
        top_errors=$(echo "$json_output" | php -r '
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json) exit;

            $identifiers = [];
            foreach ($json["files"] ?? [] as $file => $data) {
                foreach ($data["messages"] ?? [] as $msg) {
                    $id = $msg["identifier"] ?? "unknown";
                    $identifiers[$id] = ($identifiers[$id] ?? 0) + 1;
                }
            }

            if (empty($identifiers)) exit(0);

            arsort($identifiers);

            echo "\nTOP ERROR TYPES:\n";
            $count = 0;
            foreach ($identifiers as $id => $num) {
                printf("  %-55s %5d\n", $id, $num);
                $count++;
                if ($count >= 10) break;
            }
        ' 2>/dev/null)

        if [ -n "$top_errors" ]; then
            echo "$top_errors"
        fi
    fi

    # Exit with appropriate code
    if [ "$json_exit" -eq 0 ]; then
        echo ""
        echo "PHPStan analysis passed"
        exit 0
    else
        echo ""
        echo "PHPStan analysis found issues"
        exit 1
    fi
fi

# Full report mode (default)
if "$PHPSTAN_BIN" "${phpstan_args[@]}"; then
    echo "PHPStan analysis passed"
    exit 0
else
    echo "PHPStan analysis failed"
    exit 1
fi
