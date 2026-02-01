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

# Memory limit (default: 1G)
phpstan_args+=(--memory-limit=1G)

# Include component baseline if it exists
if [ -f "$COMPONENT_BASELINE" ]; then
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Using component baseline: $COMPONENT_BASELINE"
    fi
    phpstan_args+=(--baseline="$COMPONENT_BASELINE")
fi

# Include component autoloader if it exists
COMPONENT_AUTOLOAD="${PLUGIN_PATH}/vendor/autoload.php"
if [ -f "$COMPONENT_AUTOLOAD" ]; then
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Using component autoloader: $COMPONENT_AUTOLOAD"
    fi
    phpstan_args+=(--autoload-file="$COMPONENT_AUTOLOAD")
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
    # Capture stderr separately to show PHPStan errors if it fails
    stderr_file=$(mktemp)
    json_output=$("$PHPSTAN_BIN" "${phpstan_args[@]}" --error-format=json 2>"$stderr_file")
    json_exit=$?
    stderr_output=$(cat "$stderr_file")
    rm -f "$stderr_file"
    set -e

    # Parse JSON and print full summary with error details
    if [ -n "$json_output" ] && command -v php &> /dev/null; then
        parsed_output=$(echo "$json_output" | php -r '
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json) exit;

            $level = $argv[1] ?? "5";
            $componentPath = $argv[2] ?? "";

            $totals = $json["totals"] ?? [];
            $errorCount = $totals["file_errors"] ?? 0;
            $fileCount = count($json["files"] ?? []);

            if ($errorCount === 0) exit;

            // Summary header
            echo "============================================\n";
            echo "PHPSTAN SUMMARY: " . $errorCount . " errors at level " . $level . "\n";
            echo "Files with issues: " . $fileCount . "\n";
            echo "============================================\n";

            // Error details section
            echo "\nERRORS:\n";
            $identifiers = [];

            foreach ($json["files"] ?? [] as $filePath => $data) {
                // Strip component path prefix for cleaner output
                $displayPath = $filePath;
                if ($componentPath && strpos($filePath, $componentPath) === 0) {
                    $displayPath = ltrim(substr($filePath, strlen($componentPath)), "/");
                }

                foreach ($data["messages"] ?? [] as $msg) {
                    $line = $msg["line"] ?? "?";
                    $message = $msg["message"] ?? "Unknown error";
                    $identifier = $msg["identifier"] ?? "unknown";

                    echo "  " . $displayPath . ":" . $line . "\n";
                    echo "    " . $message . "\n";
                    echo "    [" . $identifier . "]\n";
                    echo "\n";

                    $identifiers[$identifier] = ($identifiers[$identifier] ?? 0) + 1;
                }
            }

            // Top error types section
            if (!empty($identifiers)) {
                arsort($identifiers);
                echo "TOP ERROR TYPES:\n";
                $count = 0;
                foreach ($identifiers as $id => $num) {
                    printf("  %-55s %5d\n", $id, $num);
                    $count++;
                    if ($count >= 10) break;
                }
            }
        ' "$PHPSTAN_LEVEL" "$PLUGIN_PATH" 2>/dev/null)

        if [ -n "$parsed_output" ]; then
            echo ""
            echo "$parsed_output"
        elif [ -n "$json_output" ]; then
            # Fallback: show raw JSON when PHP parsing fails
            echo ""
            echo "ERRORS (raw):"
            echo "$json_output"
        fi
    fi

    # Fallback: show stderr if PHPStan failed without producing JSON
    if [ "$json_exit" -ne 0 ] && [ -z "$json_output" ] && [ -n "$stderr_output" ]; then
        echo ""
        echo "PHPStan error:"
        echo "$stderr_output"
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
