#!/usr/bin/env bash
set -euo pipefail

DEFAULT_SOURCE="https://github.com/Automattic/wp-codebox.git"
TARGET=""
SOURCE="$DEFAULT_SOURCE"
REF=""
CACHE_DIR=""
NPM_BIN="npm"
DRY_RUN=0

usage() {
    cat <<'USAGE'
Usage: scripts/update-wp-codebox-cache.sh [options]

Update the WP Codebox source cache used by remote Homeboy runners.

Options:
  --target <runner-id>      Runner ID such as example-runner. Omit to run locally.
  --runner <runner-id>      Alias for --target.
  --source <git-url>        WP Codebox repository URL.
                           Default: https://github.com/Automattic/wp-codebox.git
  --ref <git-ref>           Branch, tag, or SHA to fetch/reset to. When omitted,
                           the source repository default branch is used.
  --cache-dir <path>        Runner-side checkout directory.
                           Default: ~/.cache/homeboy/wp-codebox/source
  --npm <command>           npm executable on the runner. Default: npm.
  --dry-run                 Print the runner script without executing it.
  -h, --help                Show this help.

Examples:
  scripts/update-wp-codebox-cache.sh --target example-runner
  scripts/update-wp-codebox-cache.sh --runner example-runner --ref main
  scripts/update-wp-codebox-cache.sh --source git@github.com:Automattic/wp-codebox.git --ref afe6890
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --target|--runner)
            TARGET="${2:-}"
            shift 2
            ;;
        --source)
            SOURCE="${2:-}"
            shift 2
            ;;
        --ref)
            REF="${2:-}"
            shift 2
            ;;
        --cache-dir)
            CACHE_DIR="${2:-}"
            shift 2
            ;;
        --npm)
            NPM_BIN="${2:-}"
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

if [ -z "$SOURCE" ]; then
    echo "--source must not be empty" >&2
    exit 2
fi

if [ -z "$NPM_BIN" ]; then
    echo "--npm must not be empty" >&2
    exit 2
fi

cache_update_script() {
    cat <<'REMOTE_SCRIPT'
set -euo pipefail

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

[ -n "$CACHE_DIR" ] || CACHE_DIR="${HOME}/.cache/homeboy/wp-codebox/source"

command -v git >/dev/null 2>&1 || fail "git is required to update WP Codebox cache"
command -v "$NPM_BIN" >/dev/null 2>&1 || fail "npm executable not found on runner: $NPM_BIN"

case "$NPM_BIN" in
    */*)
        NPM_DIR="$(cd "$(dirname "$NPM_BIN")" && pwd -P)" || fail "failed to resolve npm executable directory: $NPM_BIN"
        PATH="$NPM_DIR:$PATH"
        export PATH
        ;;
esac

if [ -d "$CACHE_DIR" ] && [ ! -d "$CACHE_DIR/.git" ]; then
    fail "cache dir exists but is not a git checkout: $CACHE_DIR"
fi

mkdir -p "$(dirname "$CACHE_DIR")"

if [ ! -d "$CACHE_DIR/.git" ]; then
    echo "Cloning WP Codebox cache: $SOURCE -> $CACHE_DIR"
    git clone --quiet --no-checkout "$SOURCE" "$CACHE_DIR" || fail "git clone failed for $SOURCE"
else
    echo "Updating WP Codebox cache: $CACHE_DIR"
    git -C "$CACHE_DIR" remote set-url origin "$SOURCE" || fail "failed to set WP Codebox origin URL"
fi

if [ -z "$REQUESTED_REF" ]; then
    REQUESTED_REF="$(git ls-remote --symref "$SOURCE" HEAD 2>/dev/null | sed -n 's#^ref: refs/heads/##; s#[[:space:]]HEAD$##p' | head -n 1)"
    [ -n "$REQUESTED_REF" ] || REQUESTED_REF=main
fi

echo "Fetching WP Codebox ref: $REQUESTED_REF"
git -C "$CACHE_DIR" fetch --quiet --tags origin "$REQUESTED_REF" || fail "failed to fetch ref '$REQUESTED_REF' from $SOURCE"
git -C "$CACHE_DIR" reset --hard --quiet FETCH_HEAD || fail "failed to reset cache checkout to FETCH_HEAD"

[ -f "$CACHE_DIR/package-lock.json" ] || [ -f "$CACHE_DIR/npm-shrinkwrap.json" ] || fail "WP Codebox source cache requires an npm lockfile (package-lock.json or npm-shrinkwrap.json) for deterministic npm ci: $SOURCE"

echo "Installing WP Codebox dependencies..."
"$NPM_BIN" --prefix "$CACHE_DIR" ci --include=optional --no-fund --no-audit || fail "npm ci failed in $CACHE_DIR"

echo "Building WP Codebox packages..."
"$NPM_BIN" --prefix "$CACHE_DIR" run build || fail "npm run build failed in $CACHE_DIR"

SHA="$(git -C "$CACHE_DIR" rev-parse HEAD)" || fail "failed to read resulting WP Codebox SHA"
echo "WP Codebox cache SHA: $SHA"
REMOTE_SCRIPT
}

if [ -z "$TARGET" ]; then
    if [ "$DRY_RUN" -eq 1 ]; then
        cache_update_script
        exit 0
    fi

    cache_update_script | env \
        SOURCE="$SOURCE" \
        REQUESTED_REF="$REF" \
        CACHE_DIR="$CACHE_DIR" \
        NPM_BIN="$NPM_BIN" \
        bash -s
    exit 0
fi

RUNNER_ARGS=(runner exec --script-file - --raw --env "SOURCE=$SOURCE" --env "REQUESTED_REF=$REF" --env "CACHE_DIR=$CACHE_DIR" --env "NPM_BIN=$NPM_BIN" --ssh)

if [ "$DRY_RUN" -eq 1 ]; then
    RUNNER_ARGS+=(--dry-run)
fi

RUNNER_ARGS+=("$TARGET")
cache_update_script | homeboy "${RUNNER_ARGS[@]}"
