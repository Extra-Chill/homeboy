#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$PWD"
DB_PATH="${KIMAKI_DB_PATH:-/root/.kimaki/discord-sessions.db}"
THREAD_ID=""
SESSION_ID=""
RECENT=10
SHOW_ALL=0

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
    --all)
      SHOW_ALL=1
      shift
      ;;
    *)
      PROJECT_DIR="$1"
      shift
      ;;
  esac
done

OPENCODE_CONFIG="$PROJECT_DIR/opencode.json"

echo "# Kimaki Model Inspection"
echo
echo "project_dir: $PROJECT_DIR"
echo "db_path: $DB_PATH"
[[ -n "$THREAD_ID" ]] && echo "thread_id: $THREAD_ID"
[[ -n "$SESSION_ID" ]] && echo "session_id: $SESSION_ID"
echo

echo "## Summary"
if [[ -f "$OPENCODE_CONFIG" ]]; then
  python3 - <<PY
import json
from pathlib import Path
path = Path(${OPENCODE_CONFIG@Q})
try:
    data = json.loads(path.read_text())
except Exception as exc:
    print(f"opencode_model: <parse error: {exc}>")
    raise SystemExit(0)
print(f"opencode_model: {data.get('model', '<unset>')}")
agent = data.get('agent') or {}
for name in sorted(agent):
    cfg = agent.get(name)
    if isinstance(cfg, dict) and cfg.get('model'):
        print(f"opencode_agent_{name}_model: {cfg['model']}")
PY
else
  echo "opencode_model: <missing opencode.json>"
fi

if [[ -f "$DB_PATH" ]]; then
  echo
  echo "kimaki_global_model:"
  sqlite3 -noheader "$DB_PATH" "select model_id from global_models order by updated_at desc limit 1;" || true

  if [[ -n "$THREAD_ID" && -z "$SESSION_ID" ]]; then
    SESSION_ID="$(sqlite3 -noheader "$DB_PATH" "select session_id from thread_sessions where thread_id='${THREAD_ID}' limit 1;")"
  fi

  if [[ -n "$SESSION_ID" ]]; then
    echo "kimaki_session_model:"
    sqlite3 -noheader "$DB_PATH" "select model_id from session_models where session_id='${SESSION_ID}' limit 1;" || true
    echo "kimaki_session_agent:"
    sqlite3 -noheader "$DB_PATH" "select agent_name from session_agents where session_id='${SESSION_ID}' limit 1;" || true
  fi

fi

echo
echo "## Recent mapped threads"
if [[ -f "$DB_PATH" ]]; then
  if [[ -n "$THREAD_ID" ]]; then
    sqlite3 -header -column "$DB_PATH" "select * from thread_sessions where thread_id='${THREAD_ID}' limit 1;"
  elif [[ -n "$SESSION_ID" ]]; then
    sqlite3 -header -column "$DB_PATH" "select * from thread_sessions where session_id='${SESSION_ID}' limit 5;"
  else
    sqlite3 -header -column "$DB_PATH" "select * from thread_sessions order by created_at desc limit ${RECENT};"
  fi
fi

echo
echo "## Recent model/provider events"
if [[ -f "$DB_PATH" ]]; then
  where_clause="event_json like '%modelID%' or event_json like '%providerID%'"
  if [[ -n "$THREAD_ID" ]]; then
    where_clause="thread_id='${THREAD_ID}' and (${where_clause})"
  elif [[ -n "$SESSION_ID" ]]; then
    where_clause="session_id='${SESSION_ID}' and (${where_clause})"
  fi
  sqlite3 -header -column "$DB_PATH" "select id, thread_id, session_id, timestamp, substr(event_json,1,280) as event_json from session_events where ${where_clause} order by id desc limit ${RECENT};"
fi

if [[ "$SHOW_ALL" == "1" && -f "$DB_PATH" ]]; then
  echo
  echo "## All model tables"
  sqlite3 -header -column "$DB_PATH" "select * from global_models;"
  echo
  sqlite3 -header -column "$DB_PATH" "select * from channel_models;"
  echo
  sqlite3 -header -column "$DB_PATH" "select * from session_models order by created_at desc limit 100;"
  echo
  sqlite3 -header -column "$DB_PATH" "select * from channel_agents;"
  echo
  sqlite3 -header -column "$DB_PATH" "select * from session_agents;"
fi
