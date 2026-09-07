#!/usr/bin/env bash
set -euo pipefail

# Regression smoke for homeboy-extensions#1402.
#
# A standalone/CI lint run sets HOMEBOY_ANNOTATIONS_DIR but does not export
# HOMEBOY_RUNTIME_SIDECAR_WRITER. On a CLEAN component PHPCS exits 0, yet the
# lint runner used to reach the annotations block, find no sidecar writer, and
# `exit 1` — producing the "0 findings but failed" contradiction that blocked
# every WordPress release-lint preflight.
#
# The annotations + findings sidecars are Homeboy observability output, not lint
# results. Their absence must be logged as a warning and skipped, never fatal.
# This smoke asserts:
#   - clean component, writer missing => exit 0 (the #1402 fix).
#   - real PHPCS error                => exit non-zero (the gate still works).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RUNNER="${SCRIPT_DIR}/lint-runner.sh"

# The runner sources its prelude/steps/resolve-context helpers via `:?` (they
# are hard preconditions provided by homeboy core), so provide working copies.
RUNNER_PRELUDE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RUNNER_PRELUDE runner-prelude.sh)"
RUNNER_STEPS_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RUNNER_STEPS runner-steps.sh)"
RESOLVE_CONTEXT_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

# Reproduce the #1402 failure mode (standalone/CI lint): the runtime helpers
# resolve and init succeeds, but the sidecar writer is genuinely unavailable —
# HOMEBOY_RUNTIME_SIDECAR_WRITER is unset and `homeboy_runner_init
# --sidecar-writer`'s optional fallback (${runtime_dir}/sidecar-writer.sh)
# does not exist, so homeboy_sidecar_* is never defined. We model this by
# copying the prelude/steps/resolve helpers into a runtime dir that has NO
# sidecar-writer.sh, so the prelude's runtime_dir fallback finds nothing.
RUNTIME_DIR="${TMPDIR}/runtime-no-sidecar"
mkdir -p "$RUNTIME_DIR"
cp "$RUNNER_PRELUDE_HELPER" \
   "$RUNNER_STEPS_HELPER" \
   "$RESOLVE_CONTEXT_HELPER" \
   "$RUNTIME_DIR/"
# Deliberately do NOT copy sidecar-writer.sh into $RUNTIME_DIR.

EXTENSION_DIR="${TMPDIR}/extension"
COMPONENT_DIR="${TMPDIR}/component"
ANNOTATIONS_DIR="${TMPDIR}/annotations"
OUTPUT_FILE="${TMPDIR}/lint-output.txt"

mkdir -p "${EXTENSION_DIR}/vendor/bin" "${EXTENSION_DIR}/rulesets" "${COMPONENT_DIR}" "${ANNOTATIONS_DIR}"
touch "${EXTENSION_DIR}/phpcs.xml.dist"
touch "${EXTENSION_DIR}/rulesets/homeboy-wordpress-project.xml"

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Sidecar Missing Lint Fixture
 * Text Domain: sidecar-missing-lint-fixture
 */

$alpha = 1;
PHP

# Stubbed phpcs whose report shape is controlled by PHPCS_FIXTURE_MODE:
#   clean   — 0 errors, 0 warnings, exit 0. PHPCS still emits a (non-empty) JSON
#             envelope, so the annotations block runs — the exact #1402 path
#             where a missing sidecar writer must not fail the clean lint.
#   error   — 1 real error, exit 1.
cat > "${EXTENSION_DIR}/vendor/bin/phpcs" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

for arg in "$@"; do
    if [ "$arg" = "--config-set" ]; then
        exit 0
    fi
done

mode="${PHPCS_FIXTURE_MODE:-clean}"
want_json=0
for arg in "$@"; do
    [ "$arg" = "--report=json" ] && want_json=1
done

    case "$mode" in
    clean)
        [ "$want_json" -eq 1 ] && printf '{"totals":{"errors":0,"warnings":0,"fixable":0},"files":{}}\n'
        exit 0
        ;;
    error)
        if [ "$want_json" -eq 1 ]; then
            printf '{"totals":{"errors":1,"warnings":0,"fixable":0},"files":{"%s":{"errors":1,"warnings":0,"messages":[{"message":"Inline comments must end in full-stops","source":"Squiz.Commenting.InlineComment.InvalidEndChar","severity":5,"fixable":false,"type":"ERROR","line":7,"column":1}]}}}\n' "${COMPONENT_PATH}/plugin.php"
        fi
        echo "FOUND 1 ERROR"
        exit 1
        ;;
esac
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpcs"

run_lint() {
    local mode="$1"
    : > "$OUTPUT_FILE"

    local -a env_args=(
        "HOMEBOY_EXTENSION_PATH=$EXTENSION_DIR"
        "HOMEBOY_COMPONENT_PATH=$COMPONENT_DIR"
        "HOMEBOY_COMPONENT_ID=sidecar-missing-fixture"
        "HOMEBOY_RUNTIME_RUNNER_PRELUDE=${RUNTIME_DIR}/runner-prelude.sh"
        "HOMEBOY_RUNTIME_RUNNER_STEPS=${RUNTIME_DIR}/runner-steps.sh"
        "HOMEBOY_RUNTIME_RESOLVE_CONTEXT=${RUNTIME_DIR}/resolve-context.sh"
        # HOMEBOY_RUNTIME_SIDECAR_WRITER intentionally unset; the prelude's
        # optional fallback finds no sidecar-writer.sh in $RUNTIME_DIR, so
        # homeboy_sidecar_* is never defined.
        # Setting the annotations dir forces the annotations block to run — this
        # is the code path that used to `exit 1` when the writer was missing.
        "HOMEBOY_ANNOTATIONS_DIR=$ANNOTATIONS_DIR"
        "HOMEBOY_STEP=phpcs"
        "PHPCS_FIXTURE_MODE=$mode"
    )

    set +e
    env -u HOMEBOY_RUNTIME_SIDECAR_WRITER "${env_args[@]}" "$RUNNER" >"$OUTPUT_FILE" 2>&1
    LINT_EXIT=$?
    set -e
}

# 1. Clean component, annotations dir set, sidecar writer missing => MUST pass.
#    This is the exact #1402 contradiction: PHPCS is clean (exit 0) but the
#    runner used to `exit 1` in the annotations block when the writer was gone.
run_lint clean
if [ "$LINT_EXIT" -ne 0 ]; then
    echo "FAIL: clean lint with missing sidecar writer should exit 0 (homeboy-extensions#1402), got ${LINT_EXIT}" >&2
    sed 's/^/  /' "$OUTPUT_FILE" >&2
    exit 1
fi
if grep -Fq "HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write annotations" "$OUTPUT_FILE"; then
    echo "FAIL: missing sidecar writer must not produce the fatal 'required to write annotations' error" >&2
    sed 's/^/  /' "$OUTPUT_FILE" >&2
    exit 1
fi

# 2. PHPCS reports a real error => the gate MUST still fail.
run_lint error
if [ "$LINT_EXIT" -eq 0 ]; then
    echo "FAIL: a real PHPCS error must still fail the lint gate even when the sidecar writer is missing" >&2
    sed 's/^/  /' "$OUTPUT_FILE" >&2
    exit 1
fi

echo "sidecar-writer-missing lint exit smoke passed"
