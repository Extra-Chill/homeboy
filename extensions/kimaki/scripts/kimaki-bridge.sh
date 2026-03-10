#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

if [[ $# -eq 0 ]]; then
  cat <<'EOF'
Usage: homeboy kimaki <command>

Commands:
  doctor                Run Kimaki environment diagnostics
  inspect-models        Show current Kimaki/OpenCode model state
  repro-model-fallback  Capture a compact model-fallback debug report
  raw <args...>         Pass through to kimaki CLI
EOF
  exit 0
fi

command="$1"
shift || true

case "$command" in
  doctor)
    exec bash "$SCRIPT_DIR/doctor.sh" "$@"
    ;;
  inspect-models)
    exec bash "$SCRIPT_DIR/inspect-models.sh" "$@"
    ;;
  repro-model-fallback)
    exec bash "$SCRIPT_DIR/repro-model-fallback.sh" "$@"
    ;;
  raw)
    exec kimaki "$@"
    ;;
  *)
    echo "Unknown Kimaki extension command: $command" >&2
    exit 1
    ;;
esac
