#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$PWD"
DB_PATH="${KIMAKI_DB_PATH:-/root/.kimaki/discord-sessions.db}"
LOG_PATH="${KIMAKI_LOG_PATH:-/root/.kimaki/kimaki.log}"
THREAD_ID=""
SESSION_ID=""
RECENT=20

while [[ $# -gt 0 ]]; do
  case "$1" in
    --thread)
      THREAD_ID="$2"
      shift 2
      ;;
    --session)
      SESSION_ID="$2"
      shift 2
      ;;
    --recent)
      RECENT="$2"
      shift 2
      ;;
    *)
      PROJECT_DIR="$1"
      shift
      ;;
  esac
done

echo "# Kimaki Model Fallback Repro Report"
echo
echo "This is a diagnostic snapshot, not a mutating repro."
echo

bash "$(dirname "$0")/inspect-models.sh" "$PROJECT_DIR" --recent "$RECENT" ${THREAD_ID:+--thread "$THREAD_ID"} ${SESSION_ID:+--session "$SESSION_ID"}

echo
echo "## Recent Kimaki log matches"
if [[ -f "$LOG_PATH" ]]; then
  python3 - <<PY
from pathlib import Path
path = Path(${LOG_PATH@Q})
terms = [
    'claude-opus-4-6',
    'gpt-5.4',
    'Using config model',
    'Using recent TUI model',
    'Using provider default',
    'snapshotted session model',
    'snapshotted explicit session model',
    'Channel type 11 is not supported',
    'rate limit',
    'retrying in',
]
try:
    lines = path.read_text(errors='replace').splitlines()
except Exception as exc:
    print(f'failed to read log: {exc}')
    raise SystemExit(0)
matches = [line for line in lines if any(term in line for term in terms)]
for line in matches[-${RECENT}:]:
    print(line)
PY
fi

echo
echo "## Suggested next checks"
cat <<'EOF'
- Compare DB session/channel/global model against opencode.json fallback model
- Check whether agent.build.model or agent.plan.model is pinned in opencode.json
- If runtime and DB disagree, inspect whether session prompt paths are sending both model and agent
- If behavior is thread-specific, rerun this command with --thread <thread_id>
- If behavior is session-specific, rerun this command with --session <session_id>
EOF
