#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DOCTOR="${SCRIPT_DIR}/../doctor/wp-codebox-doctor.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

FAKE_BIN="${TMPDIR}/wp-codebox"
CALLS="${TMPDIR}/calls.log"
ARCHIVE_ROOT="${TMPDIR}/archives"
mkdir -p "$ARCHIVE_ROOT"

cat > "$FAKE_BIN" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$WP_CODEBOX_CALLS"
case "$1" in
    doctor)
        printf 'WP Codebox doctor: warning\n'
        ;;
    cleanup)
        printf 'WP Codebox cleanup: ok\n'
        ;;
    *)
        printf 'unexpected mode: %s\n' "$1" >&2
        exit 64
        ;;
esac
SH
chmod +x "$FAKE_BIN"

output=$(WP_CODEBOX_CALLS="$CALLS" \
    HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
    bash "$DOCTOR" doctor --fix --stale-after-seconds 1 --archive-root "$ARCHIVE_ROOT" --json 2>&1)

if [[ "$output" != *"WP Codebox doctor: warning"* ]]; then
    echo "Expected delegated doctor output" >&2
    echo "$output" >&2
    exit 1
fi

cleanup_output=$(WP_CODEBOX_CALLS="$CALLS" \
    HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
    bash "$DOCTOR" cleanup --stale-after-seconds 0 --archive-root "$ARCHIVE_ROOT" 2>&1)

if [[ "$cleanup_output" != *"WP Codebox cleanup: ok"* ]]; then
    echo "Expected delegated cleanup output" >&2
    echo "$cleanup_output" >&2
    exit 1
fi

if ! grep -Fq "doctor --fix --stale-after-seconds 1 --archive-root $ARCHIVE_ROOT --json" "$CALLS"; then
    echo "Expected doctor args to be passed through" >&2
    cat "$CALLS" >&2
    exit 1
fi

if ! grep -Fq "cleanup --stale-after-seconds 0 --archive-root $ARCHIVE_ROOT" "$CALLS"; then
    echo "Expected cleanup args to be passed through" >&2
    cat "$CALLS" >&2
    exit 1
fi

SETTINGS_BIN="${TMPDIR}/settings-wp-codebox"
cat > "$SETTINGS_BIN" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$WP_CODEBOX_CALLS"
printf 'settings wp-codebox %s\n' "$1"
SH
chmod +x "$SETTINGS_BIN"

settings_output=$(WP_CODEBOX_CALLS="$CALLS" \
    HOMEBOY_SETTINGS_JSON="{\"wp_codebox_bin\":\"$SETTINGS_BIN\"}" \
    bash "$DOCTOR" --json 2>&1)

if [[ "$settings_output" != *"settings wp-codebox doctor"* ]]; then
    echo "Expected settings wp_codebox_bin to be used" >&2
    echo "$settings_output" >&2
    exit 1
fi

echo "WP Codebox doctor wrapper smoke passed"
