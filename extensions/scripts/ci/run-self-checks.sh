#!/usr/bin/env bash

# Run an extension's declared `self_checks` for one capability.
#
# `self_checks` in `<extension>/homeboy.json` are the commands Homeboy runs
# during the release quality preflight (`preflight.lint` / `preflight.test`).
# The published release workflow invokes `homeboy release` through
# `Extra-Chill/homeboy-action`, which always passes `--skip-checks` because it
# assumes "quality gates run in separate jobs before this script". This repo had
# no such jobs, so the declared self_checks never executed anywhere. This runner
# is that missing job.
#
# Execution semantics deliberately mirror Homeboy's own self-check runner
# (crates/homeboy-extension/src/self_check.rs): each command is executed with
# `sh -c` and the extension directory as the working directory. The one
# difference is that this runner executes every command and reports a summary
# instead of stopping at the first failure, so a pull request surfaces its whole
# blast radius in a single run.
#
# Usage:
#   scripts/ci/run-self-checks.sh <extension-dir> <capability>
#
# Environment:
#   HOMEBOY_SELF_CHECK_SKIP  Optional newline-separated list of declared
#                            commands to skip. Every skip is reported as a
#                            GitHub Actions warning so it cannot rot silently.

set -euo pipefail

EXTENSION_DIR="${1:-}"
CAPABILITY="${2:-}"

if [ -z "$EXTENSION_DIR" ] || [ -z "$CAPABILITY" ]; then
    echo "Usage: scripts/ci/run-self-checks.sh <extension-dir> <capability>" >&2
    exit 2
fi

case "$CAPABILITY" in
    lint | test) ;;
    *)
        echo "Unsupported capability '$CAPABILITY' (expected 'lint' or 'test')." >&2
        exit 2
        ;;
esac

if [ ! -d "$EXTENSION_DIR" ]; then
    echo "Extension directory not found: $EXTENSION_DIR" >&2
    exit 2
fi

MANIFEST="$EXTENSION_DIR/homeboy.json"
if [ ! -f "$MANIFEST" ]; then
    echo "Missing component manifest: $MANIFEST" >&2
    exit 2
fi

if ! jq empty "$MANIFEST" >/dev/null 2>&1; then
    echo "Component manifest is not valid JSON: $MANIFEST" >&2
    exit 1
fi

mapfile -t COMMANDS < <(jq -r --arg capability "$CAPABILITY" \
    '.self_checks[$capability] // [] | .[]' "$MANIFEST")

if [ "${#COMMANDS[@]}" -eq 0 ]; then
    echo "::notice::${EXTENSION_DIR} declares no ${CAPABILITY} self_checks — nothing to run."
    exit 0
fi

SKIPPED_COMMANDS=()
if [ -n "${HOMEBOY_SELF_CHECK_SKIP:-}" ]; then
    while IFS= read -r skip_entry; do
        [ -n "$skip_entry" ] || continue
        SKIPPED_COMMANDS+=("$skip_entry")
    done <<<"${HOMEBOY_SELF_CHECK_SKIP}"
fi

is_skipped() {
    local candidate="$1"
    local entry
    for entry in ${SKIPPED_COMMANDS[@]+"${SKIPPED_COMMANDS[@]}"}; do
        if [ "$entry" = "$candidate" ]; then
            return 0
        fi
    done
    return 1
}

echo "Running ${#COMMANDS[@]} ${CAPABILITY} self-check command(s) for ${EXTENSION_DIR}"

FAILED_COMMANDS=()
SKIPPED_COUNT=0
PASSED_COUNT=0

for command in "${COMMANDS[@]}"; do
    if is_skipped "$command"; then
        SKIPPED_COUNT=$((SKIPPED_COUNT + 1))
        echo "::warning::Skipping declared ${CAPABILITY} self-check for ${EXTENSION_DIR}: ${command}"
        continue
    fi

    echo "::group::${EXTENSION_DIR} ${CAPABILITY}: ${command}"
    command_exit=0
    (cd "$EXTENSION_DIR" && sh -c "$command") || command_exit=$?
    echo "::endgroup::"

    if [ "$command_exit" -eq 0 ]; then
        PASSED_COUNT=$((PASSED_COUNT + 1))
        echo "::notice::PASSED ${EXTENSION_DIR} ${CAPABILITY}: ${command}"
    else
        FAILED_COMMANDS+=("$command")
        echo "::error::FAILED (exit ${command_exit}) ${EXTENSION_DIR} ${CAPABILITY}: ${command}"
    fi
done

echo ""
echo "${EXTENSION_DIR} ${CAPABILITY} self-checks: ${PASSED_COUNT} passed, ${#FAILED_COMMANDS[@]} failed, ${SKIPPED_COUNT} skipped"

if [ "${#FAILED_COMMANDS[@]}" -gt 0 ]; then
    echo "Failed commands:" >&2
    for command in "${FAILED_COMMANDS[@]}"; do
        echo "  - ${command}" >&2
    done
    exit 1
fi
