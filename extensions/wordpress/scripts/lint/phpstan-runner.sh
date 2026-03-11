#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
# shellcheck source=../lib/validation-dependencies.sh
source "${DEPENDENCY_HELPER}"

# Standalone PHP static analysis script using PHPStan
# Supports summary mode via HOMEBOY_SUMMARY_MODE=1
# Supports skip via HOMEBOY_SKIP_PHPSTAN=1
# Supports level override via HOMEBOY_PHPSTAN_LEVEL (default: 5)
# NOTE: PHPStan always analyzes the full codebase (ignores HOMEBOY_LINT_GLOB)
# because it needs the complete type graph to detect collateral damage from
# changes (broken call sites, type mismatches in untouched files, etc.)

# Debug environment variables (only shown when HOMEBOY_DEBUG=1)
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: PHPStan Environment variables:"
    echo "HOMEBOY_EXTENSION_PATH=${HOMEBOY_EXTENSION_PATH:-NOT_SET}"
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
if [ -n "${HOMEBOY_EXTENSION_PATH:-}" ]; then
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH}"
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:-.}"
    PLUGIN_PATH="$COMPONENT_PATH"
else
    EXTENSION_PATH="$(dirname "$(dirname "$SCRIPT_DIR")")"
    COMPONENT_PATH="$(pwd)"
    PLUGIN_PATH="$COMPONENT_PATH"
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Extension path: $EXTENSION_PATH"
    echo "DEBUG: Plugin path: $PLUGIN_PATH"
fi

PHPSTAN_BIN="${EXTENSION_PATH}/vendor/bin/phpstan"
PHPSTAN_CONFIG="${EXTENSION_PATH}/phpstan.neon.dist"
PHPSTAN_BASE_CONFIG="$PHPSTAN_CONFIG"
COMPONENT_BASELINE="${PLUGIN_PATH}/phpstan-baseline.neon"
COMPOSITE_AUTOLOAD=""
DEPENDENCY_CONFIG=""

homeboy_mktemp() {
    local template="$1"
    local candidate_dir

    for candidate_dir in "${TMPDIR:-}" /root/tmp /var/tmp /tmp; do
        [ -z "$candidate_dir" ] && continue
        if [ -d "$candidate_dir" ] && [ -w "$candidate_dir" ]; then
            mktemp "${candidate_dir}/${template}" 2>/dev/null && return 0
        fi
    done

    mktemp 2>/dev/null
}

# Validate PHPStan exists (soft failure - not all installations have it)
if [ ! -f "$PHPSTAN_BIN" ]; then
    echo "Warning: PHPStan not found at $PHPSTAN_BIN, skipping static analysis"
    exit 0
fi

if [ ! -f "$PHPSTAN_CONFIG" ]; then
    echo "Warning: phpstan.neon.dist not found at $PHPSTAN_CONFIG, skipping static analysis"
    exit 0
fi

generate_dependency_config() {
    local tmpfile
    local has_dependencies=0

    tmpfile=$(homeboy_mktemp 'phpstan-dependencies-XXXXXX.neon')

    {
        printf '%s\n' 'includes:'
        printf '    - %s\n' "$PHPSTAN_CONFIG"
        printf '%s\n' ''
        printf '%s\n' 'parameters:'
        printf '%s\n' '    scanDirectories:'

        while IFS= read -r dependency_path; do
            [ -z "$dependency_path" ] && continue
            has_dependencies=1
            printf '        - %s\n' "$dependency_path"
        done < <(homeboy_resolve_validation_dependency_paths "$PLUGIN_PATH")
    } > "$tmpfile"

    if [ "$has_dependencies" -eq 1 ]; then
        printf '%s\n' "$tmpfile"
    else
        rm -f "$tmpfile"
        printf '%s\n' ''
    fi
}

cleanup_dependency_config() {
    [ -n "$DEPENDENCY_CONFIG" ] && rm -f "$DEPENDENCY_CONFIG"
    DEPENDENCY_CONFIG=""
}

DEPENDENCY_CONFIG=$(generate_dependency_config)
if [ -n "$DEPENDENCY_CONFIG" ] && [ -f "$DEPENDENCY_CONFIG" ]; then
    PHPSTAN_BASE_CONFIG="$DEPENDENCY_CONFIG"
fi

# Check if component has PHP files
php_file_count=$(find "$PLUGIN_PATH" -type f -name "*.php" \
    -not -path "*/vendor/*" \
    -not -path "*/node_extensions/*" \
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
phpstan_args+=(--configuration="$PHPSTAN_BASE_CONFIG")

# Level override (default: 5)
PHPSTAN_LEVEL="${HOMEBOY_PHPSTAN_LEVEL:-5}"
phpstan_args+=(--level="$PHPSTAN_LEVEL")

# Memory limit (default: 2G)
phpstan_args+=(--memory-limit=2G)

# Include component baseline if it exists
if [ -f "$COMPONENT_BASELINE" ]; then
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Using component baseline: $COMPONENT_BASELINE"
    fi
    phpstan_args+=(--baseline="$COMPONENT_BASELINE")
fi

# Include component/dependency autoloaders if they exist
generate_composite_autoload() {
    local tmpfile
    local component_autoload="${PLUGIN_PATH}/vendor/autoload.php"

    tmpfile=$(homeboy_mktemp 'homeboy-phpstan-autoload-XXXXXX.php')

    {
        printf '%s\n' '<?php'
        printf '%s\n' '$autoloadFiles = ['

        while IFS= read -r dependency_path; do
            [ -z "$dependency_path" ] && continue
            local dependency_autoload="${dependency_path}/vendor/autoload.php"
            if [ -f "$dependency_autoload" ]; then
                printf '    %s,\n' "$(printf '%s' "$dependency_autoload" | jq -Rsa .)"
            fi
        done < <(homeboy_resolve_validation_dependency_paths "$PLUGIN_PATH")

        if [ -f "$component_autoload" ]; then
            printf '    %s,\n' "$(printf '%s' "$component_autoload" | jq -Rsa .)"
        fi

        printf '%s\n' '];'
        printf '%s\n' 'foreach ($autoloadFiles as $autoloadFile) {'
        printf '%s\n' '    if (is_string($autoloadFile) && $autoloadFile !== "" && file_exists($autoloadFile)) {'
        printf '%s\n' '        require_once $autoloadFile;'
        printf '%s\n' '    }'
        printf '%s\n' '}'
    } > "$tmpfile"

    printf '%s\n' "$tmpfile"
}

cleanup_composite_autoload() {
    [ -n "$COMPOSITE_AUTOLOAD" ] && rm -f "$COMPOSITE_AUTOLOAD"
    COMPOSITE_AUTOLOAD=""
}

COMPOSITE_AUTOLOAD=$(generate_composite_autoload)

if [ -f "$COMPOSITE_AUTOLOAD" ]; then
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Using composite autoloader: $COMPOSITE_AUTOLOAD"
        echo "DEBUG: Using PHPStan config: $PHPSTAN_BASE_CONFIG"
    fi
    phpstan_args+=(--autoload-file="$COMPOSITE_AUTOLOAD")
fi

# No progress bar for cleaner output
phpstan_args+=(--no-progress)

# Thread control: HOMEBOY_PHPSTAN_THREADS overrides, otherwise auto-detect.
# On low-core machines (<=2 CPUs), force single-threaded to avoid parallel worker crashes.
# PHPStan 2.x removed the --threads CLI flag; parallel config is set via neon includes.
PHPSTAN_MAX_PROCESSES=""
if [ -n "${HOMEBOY_PHPSTAN_THREADS:-}" ]; then
    PHPSTAN_MAX_PROCESSES="${HOMEBOY_PHPSTAN_THREADS}"
elif [ "$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)" -le 2 ]; then
    PHPSTAN_MAX_PROCESSES="1"
fi

# If we need to override parallel processes, generate a temp neon config that
# includes the main config and overrides the parallel setting.
PHPSTAN_TMPCONFIG=""
generate_phpstan_config() {
    local max_processes="$1"
    local tmpfile
    tmpfile=$(homeboy_mktemp 'phpstan-XXXXXX.neon')
    cat > "$tmpfile" <<NEON
includes:
    - ${PHPSTAN_BASE_CONFIG}

parameters:
    parallel:
        maximumNumberOfProcesses: ${max_processes}
NEON
    echo "$tmpfile"
}

cleanup_phpstan_config() {
    [ -n "$PHPSTAN_TMPCONFIG" ] && rm -f "$PHPSTAN_TMPCONFIG"
    PHPSTAN_TMPCONFIG=""
}
trap 'cleanup_phpstan_config; cleanup_composite_autoload; cleanup_dependency_config' EXIT

if [ -n "$PHPSTAN_MAX_PROCESSES" ]; then
    PHPSTAN_TMPCONFIG=$(generate_phpstan_config "$PHPSTAN_MAX_PROCESSES")
    # Replace the --configuration arg with our temp config
    phpstan_args=(analyse)
    phpstan_args+=(--configuration="$PHPSTAN_TMPCONFIG")
    phpstan_args+=(--level="$PHPSTAN_LEVEL")
    phpstan_args+=(--memory-limit=2G)
    if [ -f "$COMPONENT_BASELINE" ]; then
        phpstan_args+=(--baseline="$COMPONENT_BASELINE")
    fi
    if [ -f "$COMPOSITE_AUTOLOAD" ]; then
        phpstan_args+=(--autoload-file="$COMPOSITE_AUTOLOAD")
    fi
    phpstan_args+=(--no-progress)
    phpstan_args+=("$PLUGIN_PATH")
fi

# Add the path to analyze (only when not already set by thread-override block above)
if [ -z "$PHPSTAN_MAX_PROCESSES" ]; then
    phpstan_args+=("$PLUGIN_PATH")
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: PHPStan command: $PHPSTAN_BIN ${phpstan_args[*]}"
fi

# Helper: detect parallel worker failure in PHPStan JSON output.
# Returns 0 (true) if the only errors are parallel worker crashes.
is_parallel_worker_failure() {
    local output="$1"
    [ -z "$output" ] && return 1
    echo "$output" | php -r '
        $json = json_decode(file_get_contents("php://stdin"), true);
        if (!$json) exit(1);
        $totals = $json["totals"] ?? [];
        $fileErrors = $totals["file_errors"] ?? 0;
        $globalErrors = $totals["errors"] ?? 0;
        if ($fileErrors === 0 && $globalErrors > 0) {
            foreach ($json["errors"] ?? [] as $err) {
                if (stripos($err, "parallel worker") !== false) exit(0);
            }
        }
        exit(1);
    ' 2>/dev/null
}

# Summary mode: get JSON output and parse it
if [[ "${HOMEBOY_SUMMARY_MODE:-}" == "1" ]]; then
    set +e
    # Capture stderr separately to show PHPStan errors if it fails
    stderr_file=$(homeboy_mktemp 'phpstan-stderr-XXXXXX.log')
    json_output=$("$PHPSTAN_BIN" "${phpstan_args[@]}" --error-format=json 2>"$stderr_file")
    json_exit=$?
    stderr_output=$(cat "$stderr_file")
    rm -f "$stderr_file"

    # Retry with single thread if parallel workers failed
    if [ "$json_exit" -ne 0 ] && is_parallel_worker_failure "$json_output"; then
        echo "Parallel worker failure detected, retrying single-threaded..."
        cleanup_phpstan_config
        PHPSTAN_TMPCONFIG=$(generate_phpstan_config 1)
        retry_args=(analyse --configuration="$PHPSTAN_TMPCONFIG" --level="$PHPSTAN_LEVEL" --memory-limit=2G --no-progress)
        [ -f "$COMPONENT_BASELINE" ] && retry_args+=(--baseline="$COMPONENT_BASELINE")
        [ -f "$COMPOSITE_AUTOLOAD" ] && retry_args+=(--autoload-file="$COMPOSITE_AUTOLOAD")
        retry_args+=("$PLUGIN_PATH")
        stderr_file=$(homeboy_mktemp 'phpstan-stderr-XXXXXX.log')
        json_output=$("$PHPSTAN_BIN" "${retry_args[@]}" --error-format=json 2>"$stderr_file")
        json_exit=$?
        stderr_output=$(cat "$stderr_file")
        rm -f "$stderr_file"
    fi
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

    # Write annotations sidecar JSON for CI inline comments
    if [ -n "${HOMEBOY_ANNOTATIONS_DIR:-}" ] && [ -d "${HOMEBOY_ANNOTATIONS_DIR}" ] && [ -n "$json_output" ]; then
        echo "$json_output" | php -r '
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json || empty($json["files"])) exit;
            $componentPath = $argv[1] ?? "";
            $level = $argv[2] ?? "5";
            $annotations = [];
            foreach ($json["files"] as $filePath => $data) {
                $displayPath = $filePath;
                if ($componentPath && strpos($filePath, $componentPath) === 0) {
                    $displayPath = ltrim(substr($filePath, strlen($componentPath)), "/");
                }
                foreach ($data["messages"] ?? [] as $msg) {
                    $annotations[] = [
                        "file" => $displayPath,
                        "line" => $msg["line"] ?? 0,
                        "message" => $msg["message"] ?? "Unknown",
                        "source" => "phpstan",
                        "severity" => "error",
                        "code" => $msg["identifier"] ?? "unknown",
                    ];
                }
            }
            $outDir = $argv[3] ?? "";
            if ($outDir && !empty($annotations)) {
                file_put_contents($outDir . "/phpstan.json", json_encode($annotations, JSON_PRETTY_PRINT) . "\n");
            }
        ' "$PLUGIN_PATH" "$PHPSTAN_LEVEL" "${HOMEBOY_ANNOTATIONS_DIR}" 2>/dev/null || true
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
set +e
stderr_file=$(homeboy_mktemp 'phpstan-stderr-XXXXXX.log')
"$PHPSTAN_BIN" "${phpstan_args[@]}" 2>"$stderr_file"
full_exit=$?
stderr_output=$(cat "$stderr_file")
rm -f "$stderr_file"
set -e

# Retry with single thread if parallel workers failed
if [ "$full_exit" -ne 0 ] && echo "$stderr_output" | grep -qi "parallel worker"; then
    echo "Parallel worker failure detected, retrying single-threaded..."
    cleanup_phpstan_config
    PHPSTAN_TMPCONFIG=$(generate_phpstan_config 1)
    retry_args=(analyse --configuration="$PHPSTAN_TMPCONFIG" --level="$PHPSTAN_LEVEL" --memory-limit=2G --no-progress)
    [ -f "$COMPONENT_BASELINE" ] && retry_args+=(--baseline="$COMPONENT_BASELINE")
    [ -f "$COMPOSITE_AUTOLOAD" ] && retry_args+=(--autoload-file="$COMPOSITE_AUTOLOAD")
    retry_args+=("$PLUGIN_PATH")
    "$PHPSTAN_BIN" "${retry_args[@]}"
    full_exit=$?
fi

if [ "$full_exit" -eq 0 ]; then
    echo "PHPStan analysis passed"
    exit 0
else
    # Show stderr if it wasn't already displayed
    if [ -n "$stderr_output" ]; then
        echo "$stderr_output" >&2
    fi
    echo "PHPStan analysis failed"
    exit 1
fi
