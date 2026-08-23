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
    if [ -n "${ACTIVE_BACKUP:-}" ] && [ -e "$ACTIVE_BACKUP" ]; then
        [ ! -e "$CACHE_DIR" ] || rm -rf "$CACHE_DIR" 2>/dev/null || true
        mv "$ACTIVE_BACKUP" "$CACHE_DIR" 2>/dev/null || true
    fi
    [ -n "${CANDIDATE_DIR:-}" ] && rm -rf "$CANDIDATE_DIR" 2>/dev/null || true
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
PARENT_DIR="$(dirname "$CACHE_DIR")"
LOCK_DIR="${CACHE_DIR}.update-lock"
CANDIDATE_DIR=""
ACTIVE_BACKUP=""
RELEASE_DIR=""
PREVIOUS_RELEASE_DIR=""

command -v git >/dev/null 2>&1 || fail "git is required to update WP Codebox cache"
command -v "$NPM_BIN" >/dev/null 2>&1 || fail "npm executable not found on runner: $NPM_BIN"

case "$NPM_BIN" in
    */*)
        NPM_DIR="$(cd "$(dirname "$NPM_BIN")" && pwd -P)" || fail "failed to resolve npm executable directory: $NPM_BIN"
        PATH="$NPM_DIR:$PATH"
        export PATH
        ;;
esac

mkdir -p "$PARENT_DIR"
if ! mkdir "$LOCK_DIR" 2>/dev/null; then
    fail "another WP Codebox cache update is already running: $CACHE_DIR"
fi
trap 'rmdir "$LOCK_DIR" 2>/dev/null || true' EXIT

if [ -e "$CACHE_DIR" ] && [ ! -d "$CACHE_DIR/.git" ]; then
    fail "cache dir exists but is not a git checkout: $CACHE_DIR"
fi

# Build and verify a sibling checkout. The active source remains fully usable
# until this candidate has its checkout, artifacts, identity, and readiness.
CANDIDATE_DIR="$(mktemp -d "${PARENT_DIR}/.wp-codebox-candidate.XXXXXX")" || fail "failed to create staged WP Codebox cache checkout"
echo "Staging WP Codebox cache update: $SOURCE -> $CANDIDATE_DIR"
git clone --quiet --no-checkout "$SOURCE" "$CANDIDATE_DIR" || fail "git clone failed for $SOURCE"

if [ -z "$REQUESTED_REF" ]; then
    REQUESTED_REF="$(git ls-remote --symref "$SOURCE" HEAD 2>/dev/null | sed -n 's#^ref: refs/heads/##; s#[[:space:]]HEAD$##p' | head -n 1)"
    [ -n "$REQUESTED_REF" ] || REQUESTED_REF=main
fi

echo "Fetching WP Codebox ref: $REQUESTED_REF"
git -C "$CANDIDATE_DIR" fetch --quiet --tags origin "$REQUESTED_REF" || fail "failed to fetch ref '$REQUESTED_REF' from $SOURCE"
git -C "$CANDIDATE_DIR" reset --hard --quiet FETCH_HEAD || fail "failed to reset staged cache checkout to FETCH_HEAD"
git -C "$CANDIDATE_DIR" clean -ffdx --quiet || fail "failed to clean untracked build residue from staged WP Codebox cache checkout"

[ -f "$CANDIDATE_DIR/package-lock.json" ] || [ -f "$CANDIDATE_DIR/npm-shrinkwrap.json" ] || fail "WP Codebox source cache requires an npm lockfile (package-lock.json or npm-shrinkwrap.json) for deterministic npm ci: $SOURCE"

echo "Installing WP Codebox dependencies..."
"$NPM_BIN" --prefix "$CANDIDATE_DIR" ci --include=optional --no-fund --no-audit || fail "npm ci failed in $CANDIDATE_DIR"

echo "Building WP Codebox packages..."
"$NPM_BIN" --prefix "$CANDIDATE_DIR" run build || fail "npm run build failed in $CANDIDATE_DIR"

SHA="$(git -C "$CANDIDATE_DIR" rev-parse HEAD)" || fail "failed to read resulting WP Codebox SHA"
CLI="$CANDIDATE_DIR/packages/cli/dist/index.js"
[ -x "$CLI" ] || fail "WP Codebox build did not produce executable CLI: $CLI"
VERSION="$($CLI --version 2>&1)" || fail "built WP Codebox CLI version probe failed: $CLI. Rebuild the requested ref with a compatible WP Codebox CLI."
printf '%s\n' "$VERSION" | grep -Eq '(^|[^0-9])v?0\.(2[1-9]|[3-9][0-9]|[1-9][0-9]{2,})\.[0-9]+([^0-9]|$)' || fail "built WP Codebox CLI does not satisfy the required >=0.21.0 version: ${VERSION:-unavailable}. Update the requested ref and retry."
DESCRIPTOR="$($CLI runtime descriptor --json 2>&1)" || fail "built WP Codebox CLI runtime descriptor probe failed: $CLI. Rebuild the requested ref with browser preview support."
printf '%s' "$DESCRIPTOR" | node -e 'let descriptor; try { descriptor = JSON.parse(require("fs").readFileSync(0, "utf8")); } catch { process.exit(1); } process.exit(descriptor?.schema === "wp-codebox/runtime-descriptor/v1" && descriptor?.readiness?.status === "available" && descriptor?.readiness?.browserRuntime?.status === "ready" && descriptor?.contractManifest?.schemas?.runtimeBoundary?.browserContainedSiteOpen === "wp-codebox/browser-contained-site-open/v1" ? 0 : 1);' || fail "built WP Codebox CLI is missing required browser preview capability wp-codebox/browser-contained-site-open/v1: ${DESCRIPTOR:-unavailable}. Update the requested ref and retry."
CLI_SHA256="$(sha256_file "$CLI")" || fail "failed to hash built WP Codebox CLI"
printf '%s\n' "{\"schema\":\"homeboy/wp-codebox-managed-runtime-identity/v1\",\"source_sha\":\"$SHA\",\"cli_sha256\":\"$CLI_SHA256\",\"required_capabilities\":[\"wp-codebox/browser-contained-site-open/v1\"]}" > "$CANDIDATE_DIR/.homeboy-runtime-identity.json" || fail "failed to record staged WP Codebox runtime identity"

# The stable cache path is an atomically replaced symlink to an immutable
# release directory. The update lock also protects the one-time migration from
# a legacy directory cache, so readers never use PATH while source is absent.
RELEASE_DIR="${CACHE_DIR}.releases/${SHA}.$$"
mkdir -p "$(dirname "$RELEASE_DIR")" || fail "failed to create WP Codebox release directory"
mv "$CANDIDATE_DIR" "$RELEASE_DIR" || fail "failed to stage immutable WP Codebox release"
CANDIDATE_DIR=""
NEXT_LINK="${CACHE_DIR}.next.$$"
ln -s "$RELEASE_DIR" "$NEXT_LINK" || fail "failed to create WP Codebox cache pointer"
if [ -e "$CACHE_DIR" ]; then
    PREVIOUS_RELEASE_DIR="$(readlink "$CACHE_DIR" 2>/dev/null || true)"
    ACTIVE_BACKUP="${CACHE_DIR}.previous.$$"
    mv "$CACHE_DIR" "$ACTIVE_BACKUP" || fail "failed to preserve active WP Codebox cache before promotion"
fi
mv "$NEXT_LINK" "$CACHE_DIR" || fail "failed to promote WP Codebox cache pointer"
[ -n "$ACTIVE_BACKUP" ] && rm -rf "$ACTIVE_BACKUP"
ACTIVE_BACKUP=""
case "$PREVIOUS_RELEASE_DIR" in
    "${CACHE_DIR}.releases"/*) rm -rf "$PREVIOUS_RELEASE_DIR" ;;
esac
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
