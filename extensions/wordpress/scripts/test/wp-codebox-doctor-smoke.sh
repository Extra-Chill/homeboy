#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DOCTOR="${SCRIPT_DIR}/../doctor/wp-codebox-doctor.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

FAKE_PACKAGE="${TMPDIR}/wp-codebox-package"
FAKE_BIN="${FAKE_PACKAGE}/bin/wp-codebox.js"
STALE_PROCESS_BIN="${TMPDIR}/wp-codebox-process"
ARCHIVE_ROOT="${TMPDIR}/archives"
mkdir -p "$(dirname "$FAKE_BIN")" "$ARCHIVE_ROOT"

cat > "${FAKE_PACKAGE}/package.json" <<'JSON'
{"name":"@chubes4/wp-codebox","version":"0.0.0-smoke"}
JSON

cat > "$FAKE_BIN" <<'NODE'
#!/usr/bin/env node
process.stdout.write('fake wp-codebox\n')
NODE
chmod +x "$FAKE_BIN"

cat > "$STALE_PROCESS_BIN" <<'SH'
#!/usr/bin/env bash
sleep 30
SH
chmod +x "$STALE_PROCESS_BIN"

printf 'not a zip\n' > "${ARCHIVE_ROOT}/corrupt-playground.zip"

"$STALE_PROCESS_BIN" recipe-run &
STALE_PID=$!
trap 'kill "$STALE_PID" >/dev/null 2>&1 || true; rm -rf "$TMPDIR"' EXIT

output=$(HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
    HOMEBOY_WP_CODEBOX_ARCHIVE_ROOTS="$ARCHIVE_ROOT" \
    bash "$DOCTOR" doctor --stale-after-seconds 1 2>&1)

if [[ "$output" != *"WP Codebox runner doctor"* ]]; then
    echo "Expected doctor header" >&2
    echo "$output" >&2
    exit 1
fi

if [[ "$output" != *"wp-codebox.binary"* ]] || [[ "$output" != *"sha256:"* ]]; then
    echo "Expected binary sha diagnostics" >&2
    echo "$output" >&2
    exit 1
fi

if [[ "$output" != *"wp-codebox.source"* ]]; then
    echo "Expected source diagnostics" >&2
    echo "$output" >&2
    exit 1
fi

if [[ "$output" != *"corrupt archive"* ]]; then
    echo "Expected corrupt archive warning" >&2
    echo "$output" >&2
    exit 1
fi

cleanup_output=$(HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
    HOMEBOY_WP_CODEBOX_ARCHIVE_ROOTS="$ARCHIVE_ROOT" \
    bash "$DOCTOR" cleanup --stale-after-seconds 0 2>&1)

if [[ "$cleanup_output" != *"removed corrupt archive"* ]]; then
    echo "Expected corrupt archive cleanup" >&2
    echo "$cleanup_output" >&2
    exit 1
fi

if [[ "$cleanup_output" != *"sent TERM to stale recipe-run pid"* ]]; then
    echo "Expected stale recipe-run process cleanup" >&2
    echo "$cleanup_output" >&2
    exit 1
fi

if [ -e "${ARCHIVE_ROOT}/corrupt-playground.zip" ]; then
    echo "Expected cleanup to remove corrupt archive" >&2
    exit 1
fi

for _ in 1 2 3 4 5; do
    if ! kill -0 "$STALE_PID" >/dev/null 2>&1; then
        break
    fi
    sleep 0.1
done

if kill -0 "$STALE_PID" >/dev/null 2>&1; then
    echo "Expected cleanup to terminate stale recipe-run process" >&2
    exit 1
fi

echo "WP Codebox doctor smoke passed"
