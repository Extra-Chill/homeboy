#!/usr/bin/env bash
set -euo pipefail

DEFAULT_REPO="https://github.com/Extra-Chill/homeboy-extensions"
DEFAULT_EXTENSIONS="nodejs rust wordpress go swift"

TARGET=""
REPO="$DEFAULT_REPO"
HOMEBOY_BIN="homeboy"
EXTENSIONS="$DEFAULT_EXTENSIONS"
DRY_RUN=0

usage() {
    cat <<'USAGE'
Usage: scripts/bootstrap-standard-extensions.sh [options]

Install the standard Homeboy extension set on the local machine or an SSH
reachable remote runner.

Options:
  --target <runner-id>        Runner ID such as homeboy-lab. Omit for local bootstrap.
  --extensions "<ids>"       Space-separated extension IDs to install.
                             Default: nodejs rust wordpress go swift.
  --repo <url-or-path>        Extension monorepo URL or path.
                             Default: https://github.com/Extra-Chill/homeboy-extensions.
  --homeboy <command>         Homeboy executable on the target.
                             Default: homeboy.
  --dry-run                  Print the target script without executing it.
  -h, --help                 Show this help.

Examples:
  scripts/bootstrap-standard-extensions.sh --target homeboy-lab
  scripts/bootstrap-standard-extensions.sh --target chubes@homeboy-lab --extensions "nodejs rust wordpress"
  scripts/bootstrap-standard-extensions.sh --repo /path/to/homeboy-extensions --extensions "rust" --dry-run
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --target)
            TARGET="${2:-}"
            shift 2
            ;;
        --extensions)
            EXTENSIONS="${2:-}"
            shift 2
            ;;
        --repo)
            REPO="${2:-}"
            shift 2
            ;;
        --homeboy)
            HOMEBOY_BIN="${2:-}"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [ -z "$EXTENSIONS" ]; then
    echo "--extensions must not be empty" >&2
    exit 2
fi

if [ -z "$REPO" ]; then
    echo "--repo must not be empty" >&2
    exit 2
fi

if [ -z "$HOMEBOY_BIN" ]; then
    echo "--homeboy must not be empty" >&2
    exit 2
fi

RUNNER_ID="${TARGET:-local}"
RUNNER_ARGS=(runner exec "$RUNNER_ID" --script-file - --raw --env "HOMEBOY_BIN=$HOMEBOY_BIN" --env "REPO=$REPO" --env "EXTENSIONS=$EXTENSIONS")

if [ -n "$TARGET" ]; then
    RUNNER_ARGS+=(--ssh)
fi

if [ "$DRY_RUN" -eq 1 ]; then
    RUNNER_ARGS+=(--dry-run)
fi

"$HOMEBOY_BIN" "${RUNNER_ARGS[@]}" <<'REMOTE_SCRIPT'
set -euo pipefail

command -v "$HOMEBOY_BIN" >/dev/null 2>&1 || {
    echo "Homeboy executable not found on target: $HOMEBOY_BIN" >&2
    exit 127
}

for EXTENSION_ID in $EXTENSIONS; do
    echo "Installing Homeboy extension: $EXTENSION_ID"
    "$HOMEBOY_BIN" extension install "$REPO" --id "$EXTENSION_ID"
    "$HOMEBOY_BIN" extension show "$EXTENSION_ID" >/dev/null
done

echo "Installed Homeboy extensions:"
"$HOMEBOY_BIN" extension list
REMOTE_SCRIPT
