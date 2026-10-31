#!/usr/bin/env bash
set -euo pipefail

DEFAULT_REPO="https://github.com/Extra-Chill/homeboy-extensions"
DEFAULT_EXTENSIONS="nodejs rust wordpress go swift"

TARGET=""
REPO="$DEFAULT_REPO"
HOMEBOY_BIN="homeboy"
EXTENSIONS="$DEFAULT_EXTENSIONS"
DRY_RUN=0
REPLACE_EXISTING=0

usage() {
    cat <<'USAGE'
Usage: scripts/bootstrap-standard-extensions.sh [options]

Install the standard Homeboy extension set locally or on a Homeboy runner.

Options:
  --target <runner-id>        Runner ID such as example-runner. Omit for local bootstrap.
  --extensions "<ids>"       Space-separated extension IDs to install.
                             Default: nodejs rust wordpress go swift.
  --repo <url-or-path>        Extension monorepo URL or path.
                             Default: https://github.com/Extra-Chill/homeboy-extensions.
  --homeboy <command>         Homeboy executable on the target.
                              Default: homeboy.
  --replace-existing          Replace existing extension installs with managed
                              installs from --repo. Use for explicit runner
                              repair; linked source checkouts are preserved.
  --dry-run                  Print the target script without executing it.
  -h, --help                 Show this help.

Examples:
  scripts/bootstrap-standard-extensions.sh --target example-runner
  scripts/bootstrap-standard-extensions.sh --target operator@example-runner --extensions "nodejs rust wordpress"
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
        --replace-existing)
            REPLACE_EXISTING=1
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

REMOTE_SCRIPT_CONTENT="$(cat <<'REMOTE_SCRIPT'
set -euo pipefail

command -v "$HOMEBOY_BIN" >/dev/null 2>&1 || {
    echo "Homeboy executable not found on target: $HOMEBOY_BIN" >&2
    exit 127
}

for EXTENSION_ID in $EXTENSIONS; do
    echo "Installing Homeboy extension: $EXTENSION_ID"
    INSTALL_ARGS=(extension install "$REPO" --id "$EXTENSION_ID")
    if [ "${REPLACE_EXISTING:-0}" = "1" ]; then
        INSTALL_ARGS+=(--replace)
    fi
    "$HOMEBOY_BIN" "${INSTALL_ARGS[@]}"
    "$HOMEBOY_BIN" extension show "$EXTENSION_ID" >/dev/null
done

echo "Installed Homeboy extensions:"
"$HOMEBOY_BIN" extension list
REMOTE_SCRIPT
)"

if [ -n "$TARGET" ]; then
    RUNNER_ID="$TARGET"
    CONTROLLER_HOMEBOY_BIN="${HOMEBOY_CONTROLLER_BIN:-homeboy}"
    RUNNER_ARGS=(runner exec "$RUNNER_ID" --script-file - --raw --env "HOMEBOY_BIN=$HOMEBOY_BIN" --env "REPO=$REPO" --env "EXTENSIONS=$EXTENSIONS" --env "REPLACE_EXISTING=$REPLACE_EXISTING")

    if [ "$DRY_RUN" -eq 1 ]; then
        RUNNER_ARGS+=(--dry-run)
    fi

    printf '%s\n' "$REMOTE_SCRIPT_CONTENT" | "$CONTROLLER_HOMEBOY_BIN" "${RUNNER_ARGS[@]}"
    exit $?
fi

if [ "$DRY_RUN" -eq 1 ]; then
    printf '# Target: local\n'
    printf "HOMEBOY_BIN='%s'\n" "$HOMEBOY_BIN"
    printf "REPO='%s'\n" "$REPO"
    printf "EXTENSIONS='%s'\n" "$EXTENSIONS"
    printf "REPLACE_EXISTING='%s'\n" "$REPLACE_EXISTING"
    printf '%s\n' "$REMOTE_SCRIPT_CONTENT"
    exit 0
fi

printf '%s\n' "$REMOTE_SCRIPT_CONTENT" | HOMEBOY_BIN="$HOMEBOY_BIN" REPO="$REPO" EXTENSIONS="$EXTENSIONS" REPLACE_EXISTING="$REPLACE_EXISTING" bash
