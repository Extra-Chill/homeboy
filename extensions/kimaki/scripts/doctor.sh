#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="${1:-$PWD}"
DB_PATH="${KIMAKI_DB_PATH:-/root/.kimaki/discord-sessions.db}"
LOG_PATH="${KIMAKI_LOG_PATH:-/root/.kimaki/kimaki.log}"
OPENCODE_CONFIG="$PROJECT_DIR/opencode.json"

echo "# Kimaki Doctor"
echo
echo "project_dir: $PROJECT_DIR"
echo "db_path: $DB_PATH"
echo "log_path: $LOG_PATH"
echo

echo "## Binaries"
command -v kimaki >/dev/null 2>&1 && echo "kimaki: $(command -v kimaki)" || echo "kimaki: missing"
command -v opencode >/dev/null 2>&1 && echo "opencode: $(command -v opencode)" || echo "opencode: missing"
command -v sqlite3 >/dev/null 2>&1 && echo "sqlite3: $(command -v sqlite3)" || echo "sqlite3: missing"
command -v jq >/dev/null 2>&1 && echo "jq: $(command -v jq)" || echo "jq: missing (optional)"

echo
echo "## Service"
if command -v systemctl >/dev/null 2>&1; then
  systemctl is-active kimaki 2>/dev/null || true
else
  echo "systemctl unavailable"
fi

echo
echo "## Files"
[[ -f "$DB_PATH" ]] && echo "db: present" || echo "db: missing"
[[ -f "$LOG_PATH" ]] && echo "log: present" || echo "log: missing"
[[ -f "$OPENCODE_CONFIG" ]] && echo "opencode.json: present" || echo "opencode.json: missing"

echo
echo "## Projects"
kimaki project list --json 2>/dev/null || true

echo
echo "## Recent log tail"
if [[ -f "$LOG_PATH" ]]; then
  tail -n 40 "$LOG_PATH"
fi
