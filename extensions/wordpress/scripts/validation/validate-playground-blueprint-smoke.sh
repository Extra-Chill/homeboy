#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-blueprint-validation.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT

BLUEPRINT_FILE="${TMPDIR}/blueprint.json"
ARGS_FILE="${TMPDIR}/wp-codebox-args.txt"
ARTIFACT_DIR="${TMPDIR}/artifacts"
FAKE_WP_CODEBOX="${TMPDIR}/wp-codebox"

cat > "$BLUEPRINT_FILE" <<'JSON'
{"steps":[]}
JSON

cat > "$FAKE_WP_CODEBOX" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = '--version' ]; then
    printf '0.21.0\n'
    exit 0
fi
if [ "${1:-}" = runtime ]; then
    printf '%s\n' '{"schema":"wp-codebox/runtime-descriptor/v1","readiness":{"status":"available","browserRuntime":{"status":"ready"}},"contractManifest":{"schemas":{"runtimeBoundary":{"browserContainedSiteOpen":"wp-codebox/browser-contained-site-open/v1"}}}}'
    exit 0
fi
printf '%s\n' "$@" > "${WP_CODEBOX_ARGS_FILE}"
printf 'WP Codebox blueprint validation\n'
SH
chmod +x "$FAKE_WP_CODEBOX"

HOMEBOY_WP_CODEBOX_BIN="$FAKE_WP_CODEBOX" \
WP_CODEBOX_ARGS_FILE="$ARGS_FILE" \
    bash "${SCRIPT_DIR}/validate-playground-blueprint.sh" "$BLUEPRINT_FILE" --wp latest --php 8.3 --artifact-dir "$ARTIFACT_DIR" > "${TMPDIR}/stdout.txt"

if ! grep -q '^validate-blueprint$' "$ARGS_FILE"; then
    echo "Expected validate-playground-blueprint.sh to invoke wp-codebox validate-blueprint" >&2
    exit 1
fi
if ! grep -q '^--blueprint$' "$ARGS_FILE" || ! grep -q "$BLUEPRINT_FILE" "$ARGS_FILE"; then
    echo "Expected blueprint path to be forwarded to wp-codebox" >&2
    exit 1
fi
if ! grep -q '^--artifacts$' "$ARGS_FILE" || ! grep -q "$ARTIFACT_DIR" "$ARGS_FILE"; then
    echo "Expected artifact directory to be forwarded to wp-codebox" >&2
    exit 1
fi
if grep -q -- '--php' "$ARGS_FILE"; then
    echo "Did not expect --php to be forwarded; wp-codebox owns the runtime PHP version" >&2
    exit 1
fi

echo "validate playground blueprint wp-codebox smoke passed"
