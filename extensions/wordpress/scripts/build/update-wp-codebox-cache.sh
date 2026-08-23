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
    if [ -n "${IDENTITY_BACKUP:-}" ] && [ -n "${IDENTITY_PATH:-}" ]; then
        cp "$IDENTITY_BACKUP" "$IDENTITY_PATH" 2>/dev/null || true
        rm -f "$IDENTITY_BACKUP"
    fi
    echo "ERROR: $*" >&2
    exit 1
}

sha256_file() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        fail "sha256 tool is required (shasum or sha256sum)"
    fi
}

[ -n "$CACHE_DIR" ] || CACHE_DIR="${HOME}/.cache/homeboy/wp-codebox/source"
IDENTITY_PATH="$CACHE_DIR/.homeboy-runtime-identity.json"
IDENTITY_BACKUP=""

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

# The identity is untracked build metadata and `git clean` removes it. Retain
# it until the replacement CLI has passed every readiness probe so a failed
# update cannot leave the managed cache without its last known identity.
if [ -f "$IDENTITY_PATH" ]; then
    IDENTITY_BACKUP="$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-identity.XXXXXX")" || fail "failed to reserve managed runtime identity backup"
    cp "$IDENTITY_PATH" "$IDENTITY_BACKUP" || fail "failed to back up managed runtime identity"
fi

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
git -C "$CACHE_DIR" clean -ffdx --quiet || fail "failed to clean untracked build residue from WP Codebox cache checkout"

[ -f "$CACHE_DIR/package-lock.json" ] || [ -f "$CACHE_DIR/npm-shrinkwrap.json" ] || fail "WP Codebox source cache requires an npm lockfile (package-lock.json or npm-shrinkwrap.json) for deterministic npm ci: $SOURCE"

echo "Installing WP Codebox dependencies..."
"$NPM_BIN" --prefix "$CACHE_DIR" ci --include=optional --no-fund --no-audit || fail "npm ci failed in $CACHE_DIR"

echo "Building WP Codebox packages..."
"$NPM_BIN" --prefix "$CACHE_DIR" run build || fail "npm run build failed in $CACHE_DIR"

SHA="$(git -C "$CACHE_DIR" rev-parse HEAD)" || fail "failed to read resulting WP Codebox SHA"
CLI="$CACHE_DIR/packages/cli/dist/index.js"
[ -x "$CLI" ] || fail "WP Codebox build did not produce executable CLI: $CLI"
VERSION="$($CLI --version 2>&1)" || fail "built WP Codebox CLI version probe failed: $CLI. Rebuild the requested ref with a compatible WP Codebox CLI."
printf '%s\n' "$VERSION" | grep -Eq '(^|[^0-9])v?0\.(2[1-9]|[3-9][0-9]|[1-9][0-9]{2,})\.[0-9]+([^0-9]|$)' || fail "built WP Codebox CLI does not satisfy the required >=0.21.0 version: ${VERSION:-unavailable}. Update the requested ref and retry."
DESCRIPTOR="$($CLI runtime descriptor --json 2>&1)" || fail "built WP Codebox CLI runtime descriptor probe failed: $CLI. Rebuild the requested ref with browser preview support."
printf '%s' "$DESCRIPTOR" | node -e 'let descriptor; try { descriptor = JSON.parse(require("fs").readFileSync(0, "utf8")); } catch { process.exit(1); } process.exit(descriptor?.schema === "wp-codebox/runtime-descriptor/v1" && descriptor?.readiness?.status === "available" && descriptor?.readiness?.browserRuntime?.status === "ready" && descriptor?.contractManifest?.schemas?.runtimeBoundary?.browserContainedSiteOpen === "wp-codebox/browser-contained-site-open/v1" ? 0 : 1);' || fail "built WP Codebox CLI is missing required browser preview capability wp-codebox/browser-contained-site-open/v1: ${DESCRIPTOR:-unavailable}. Update the requested ref and retry."
CLI_SHA256="$(sha256_file "$CLI")" || fail "failed to hash built WP Codebox CLI"
printf '%s\n' "{\"schema\":\"homeboy/wp-codebox-managed-runtime-identity/v1\",\"source_sha\":\"$SHA\",\"cli_sha256\":\"$CLI_SHA256\",\"required_capabilities\":[\"wp-codebox/browser-contained-site-open/v1\"]}" > "$IDENTITY_PATH" || fail "failed to record managed WP Codebox runtime identity"
rm -f "$IDENTITY_BACKUP"
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

RUNNER_ARGS=(runner exec --script-file - --raw --env "SOURCE=$SOURCE" --env "REQUESTED_REF=$REF" --env "CACHE_DIR=$CACHE_DIR" --env "NPM_BIN=$NPM_BIN")

if [ "$DRY_RUN" -eq 1 ]; then
    RUNNER_ARGS+=(--dry-run)
fi

RUNNER_ARGS+=("$TARGET")
cache_update_script | homeboy "${RUNNER_ARGS[@]}"
