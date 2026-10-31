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
RELEASE_DIR=""

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

if [ -z "$REQUESTED_REF" ]; then
    REQUESTED_REF="$(git ls-remote --symref "$SOURCE" HEAD 2>/dev/null | sed -n 's#^ref: refs/heads/##; s#[[:space:]]HEAD$##p' | head -n 1)"
    [ -n "$REQUESTED_REF" ] || REQUESTED_REF=main
fi

# Resolve the ref without cloning so an already-built release is reused for
# the cost of one ls-remote. Peeled tags win over the tag object.
SHA=""
if printf '%s' "$REQUESTED_REF" | grep -Eq '^[0-9a-f]{40}$'; then
    SHA="$REQUESTED_REF"
else
    SHA="$(git ls-remote "$SOURCE" "refs/heads/$REQUESTED_REF" "refs/tags/$REQUESTED_REF" "refs/tags/$REQUESTED_REF^{}" 2>/dev/null | sort -k2 -r | awk 'NR==1{print $1}')"
fi

# Reuse only releases whose recorded source and CLI bytes still match.
for EXISTING_RELEASE in "${CACHE_DIR}.releases/"*; do
    [ -d "$EXISTING_RELEASE" ] && [ ! -L "$EXISTING_RELEASE" ] || continue
    [ -n "$SHA" ] || break
    if node - "$EXISTING_RELEASE" "$SHA" <<'NODE'
const fs = require('node:fs');
const crypto = require('node:crypto');
const [release, sha] = process.argv.slice(2);
try {
  const identity = JSON.parse(fs.readFileSync(`${release}/.homeboy-runtime-identity.json`, 'utf8'));
  const cli = `${release}/packages/cli/dist/index.js`;
  fs.accessSync(cli, fs.constants.X_OK);
  const hash = crypto.createHash('sha256').update(fs.readFileSync(cli)).digest('hex');
  if (identity.schema !== 'homeboy/wp-codebox-managed-runtime-identity/v1' || identity.source_sha !== sha || identity.cli_sha256 !== hash) process.exit(1);
} catch { process.exit(1); }
NODE
    then
        RELEASE_DIR="$EXISTING_RELEASE"
        echo "Reusing verified WP Codebox release: $RELEASE_DIR"
        break
    fi
done

if [ -z "$RELEASE_DIR" ]; then
# Build and verify a sibling checkout. The active source remains fully usable
# until this candidate has its checkout, artifacts, identity, and readiness.
CANDIDATE_DIR="$(mktemp -d "${PARENT_DIR}/.wp-codebox-candidate.XXXXXX")" || fail "failed to create staged WP Codebox cache checkout"
echo "Staging WP Codebox cache update: $SOURCE -> $CANDIDATE_DIR"
git clone --quiet --no-checkout --filter=blob:none -- "$SOURCE" "$CANDIDATE_DIR" || fail "git clone failed for $SOURCE"

echo "Fetching WP Codebox ref: $REQUESTED_REF"
git -C "$CANDIDATE_DIR" fetch --quiet --tags origin "$REQUESTED_REF" || fail "failed to fetch ref '$REQUESTED_REF' from $SOURCE"
git -C "$CANDIDATE_DIR" reset --hard --quiet FETCH_HEAD || fail "failed to reset staged cache checkout to FETCH_HEAD"
SHA="$(git -C "$CANDIDATE_DIR" rev-parse HEAD)" || fail "failed to read staged WP Codebox SHA"
export WP_CODEBOX_SOURCE_REF="$REQUESTED_REF" WP_CODEBOX_SOURCE_SHA="$SHA"
git -C "$CANDIDATE_DIR" clean -ffdx --quiet || fail "failed to clean untracked build residue from staged WP Codebox cache checkout"

[ -f "$CANDIDATE_DIR/package-lock.json" ] || [ -f "$CANDIDATE_DIR/npm-shrinkwrap.json" ] || fail "WP Codebox source cache requires an npm lockfile (package-lock.json or npm-shrinkwrap.json) for deterministic npm ci: $SOURCE"

echo "Installing WP Codebox dependencies..."
"$NPM_BIN" --prefix "$CANDIDATE_DIR" ci --include=optional --no-fund --no-audit || fail "npm ci failed in $CANDIDATE_DIR"

echo "Building WP Codebox packages..."
"$NPM_BIN" --prefix "$CANDIDATE_DIR" run build || fail "npm run build failed in $CANDIDATE_DIR"

CLI="$CANDIDATE_DIR/packages/cli/dist/index.js"
[ -x "$CLI" ] || fail "WP Codebox build did not produce executable CLI: $CLI"
VERSION="$($CLI --version 2>&1)" || fail "built WP Codebox CLI version probe failed: $CLI. Rebuild the requested ref with a compatible WP Codebox CLI."
DESCRIPTOR="$($CLI runtime descriptor --json 2>&1)" || fail "built WP Codebox CLI runtime descriptor probe failed: $CLI. Rebuild the requested ref with browser preview support."
PREFLIGHT_OUTPUT="$(node - "$VERSION" 3<<<"$DESCRIPTOR" <<'NODE'
const versionOutput = process.argv[2];
const descriptorOutput = require('node:fs').readFileSync(3, 'utf8');
const versionMatch = versionOutput.match(/\bv?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\b/);
const version = versionMatch ? `${versionMatch[1]}.${versionMatch[2]}.${versionMatch[3]}${versionMatch[4] ? `-${versionMatch[4]}` : ''}` : '';
const minimum = [0, 21, 0];
const parts = versionMatch ? versionMatch.slice(1, 4).map(Number) : [];
const comparison = parts.reduce((result, part, index) => result || part - minimum[index], 0);
const tooOld = !version || comparison < 0 || Boolean(versionMatch?.[4] && comparison === 0);
if (tooOld) {
  process.stdout.write(`WP Codebox wp_codebox_version_too_old: required >=0.21.0, observed ${version || 'unavailable'}.\n`);
  process.exit(1);
}
try {
  const descriptor = JSON.parse(descriptorOutput);
  if (descriptor?.schema !== 'wp-codebox/runtime-descriptor/v1' || descriptor?.contractManifest?.schemas?.runtimeBoundary?.browserContainedSiteOpen !== 'wp-codebox/browser-contained-site-open/v1') throw new Error();
} catch {
  process.stdout.write(`WP Codebox wp_codebox_browser_preview_capability_missing: required >=0.21.0, observed ${version}.\n`);
  process.exit(1);
}
NODE
)" || fail "${PREFLIGHT_OUTPUT:-built WP Codebox CLI preflight failed}. Update the requested ref and retry."
CLI_SHA256="$(sha256_file "$CLI")" || fail "failed to hash built WP Codebox CLI"
printf '%s\n' "{\"schema\":\"homeboy/wp-codebox-managed-runtime-identity/v1\",\"source_sha\":\"$SHA\",\"cli_sha256\":\"$CLI_SHA256\",\"required_capabilities\":[\"wp-codebox/browser-contained-site-open/v1\"]}" > "$CANDIDATE_DIR/.homeboy-runtime-identity.json" || fail "failed to record staged WP Codebox runtime identity"

# Publish new builds as immutable releases; reused releases take the same
# atomic promotion path below.
RELEASE_DIR="${CACHE_DIR}.releases/${SHA}.$$"
mkdir -p "$(dirname "$RELEASE_DIR")" || fail "failed to create WP Codebox release directory"
mv "$CANDIDATE_DIR" "$RELEASE_DIR" || fail "failed to stage immutable WP Codebox release"
CANDIDATE_DIR=""
fi
PREVIOUS_RELEASE="$(readlink "$CACHE_DIR" 2>/dev/null || true)"
NEXT_LINK="${CACHE_DIR}.next.$$"
ln -s "$RELEASE_DIR" "$NEXT_LINK" || fail "failed to create WP Codebox cache pointer"
if [ ! -L "$CACHE_DIR" ] && [ -e "$CACHE_DIR" ]; then
    # A legacy directory cannot be replaced by a symlink with rename(2). The
    # update lock makes this one-time migration fail closed for new readers.
    LEGACY_BACKUP="${CACHE_DIR}.legacy.$$"
    mv "$CACHE_DIR" "$LEGACY_BACKUP" || fail "failed to preserve legacy WP Codebox cache"
fi
# Test seam: pause after the reader can resolve the old immutable release and
# immediately before the atomic replacement below.
if [ -n "${HOMEBOY_WP_CODEBOX_PROMOTION_READY_FILE:-}" ]; then
    : > "$HOMEBOY_WP_CODEBOX_PROMOTION_READY_FILE"
    while [ ! -e "${HOMEBOY_WP_CODEBOX_PROMOTION_RELEASE_FILE:-}" ]; do sleep 0.01; done
fi
# `mv` follows a destination symlink-to-directory on some platforms. Node's
# rename maps directly to rename(2), replacing the symlink entry itself.
node -e 'require("node:fs").renameSync(process.argv[1], process.argv[2])' "$NEXT_LINK" "$CACHE_DIR" || fail "failed to promote WP Codebox cache pointer"
# Under the update lock, keep current and previous targets, releases modified
# in the last hour, and releases with live open files/cwd/executables. Without
# lsof (or when inspection fails), retain releases rather than risk readers.
if command -v lsof >/dev/null 2>&1; then
    for OLD_RELEASE in "${CACHE_DIR}.releases/"*; do
        [ -d "$OLD_RELEASE" ] && [ ! -L "$OLD_RELEASE" ] || continue
        [ "$OLD_RELEASE" != "$RELEASE_DIR" ] && [ "$OLD_RELEASE" != "$PREVIOUS_RELEASE" ] || continue
        RECENT="$(find "$OLD_RELEASE" -mmin -60 -print -quit 2>&1)" || continue
        [ -z "$RECENT" ] || continue
        OPEN_FILES="$(lsof -w -nP +D "$OLD_RELEASE" 2>&1)" && continue
        [ "$?" -eq 1 ] && [ -z "$OPEN_FILES" ] || continue
        rm -rf "$OLD_RELEASE"
    done
fi
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
