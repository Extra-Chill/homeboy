#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="${SCRIPT_DIR}/update-wp-codebox-cache.sh"
REAL_NODE="$(command -v node)"
export REAL_NODE
TMPDIR="${TMPDIR:-/tmp}"
WORK_DIR="$(mktemp -d "${TMPDIR%/}/homeboy-wp-codebox-cache.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

REMOTE_REPO="${WORK_DIR}/wp-codebox.git"
SOURCE_WORK="${WORK_DIR}/source-work"
CACHE_DIR="${WORK_DIR}/cache/source"
FAKE_BIN="${WORK_DIR}/bin"

mkdir -p "$FAKE_BIN"

cat > "${FAKE_BIN}/npm" <<'NPM'
#!/usr/bin/env node
NPM

cat > "${FAKE_BIN}/node" <<'NODE'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = '-e' ] || [ "${1:-}" = '-' ] || [[ "${1:-}" = *.cjs ]]; then
    exec "$REAL_NODE" "$@"
fi
if [[ "${1:-}" = */packages/cli/dist/index.js ]]; then
    cli="$1"
    shift
    source_root="$(dirname "$(dirname "$(dirname "$(dirname "$cli")")")")"
    if [ -f "${source_root}/incompatible" ] && grep -q '^incompatible$' "${source_root}/incompatible"; then
        version='0.20.0'
        descriptor='{}'
    elif [ -f "${source_root}/minimum-compatible" ]; then
        version='0.21.0'
        descriptor='{"schema":"wp-codebox/runtime-descriptor/v1","readiness":{"status":"unavailable","browserRuntime":{"status":"unavailable"}},"contractManifest":{"schemas":{"runtimeBoundary":{"browserContainedSiteOpen":"wp-codebox/browser-contained-site-open/v1"}}}}'
    else
        version='0.23.4'
        descriptor='{"schema":"wp-codebox/runtime-descriptor/v1","readiness":{"status":"unavailable","browserRuntime":{"status":"unavailable"}},"contractManifest":{"schemas":{"runtimeBoundary":{"browserContainedSiteOpen":"wp-codebox/browser-contained-site-open/v1"}}}}'
    fi
    case "${*}" in
        --version) printf '%s\n' "$version" ;;
        'runtime descriptor --json') printf '%s\n' "$descriptor" ;;
        *) exit 1 ;;
    esac
    exit 0
fi
shift
prefix=""
args=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            prefix="${2:-}"
            shift 2
            ;;
        *)
            args+=("$1")
            shift
            ;;
    esac
done
[ -n "$prefix" ] || { echo "missing --prefix" >&2; exit 2; }
case "${args[*]}" in
    ci*)
        case " ${args[*]} " in
            *" --include=optional "*) ;;
            *)
                echo "expected npm ci to include optional dependencies: ${args[*]}" >&2
                exit 2
                ;;
        esac
        touch "${prefix}/npm-install-ran"
        ;;
    "run build")
        if [ -e "${prefix}/packages/cli/dist/index.js" ]; then
            echo "stale CLI build output survived cache cleanup" >&2
            exit 2
        fi
        mkdir -p "${prefix}/node_modules/@automattic/wp-codebox-core/dist"
        mkdir -p "${prefix}/packages/cli/dist"
        printf '%s\n' 'built' > "${prefix}/node_modules/@automattic/wp-codebox-core/dist/index.js"
        cat > "${prefix}/packages/cli/dist/index.js" <<'CLI'
#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const source = path.resolve(__dirname, '../../..');
const version = fs.existsSync(path.join(source, 'incompatible')) ? '0.20.0' : fs.existsSync(path.join(source, 'minimum-compatible')) ? '0.21.0' : '0.23.4';
const descriptor = version === '0.20.0' ? {} : {
  schema: 'wp-codebox/runtime-descriptor/v1',
  readiness: { status: 'unavailable', browserRuntime: { status: 'unavailable' } },
  contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } },
};
if (process.argv.includes('--version')) process.stdout.write(`${version}\n`);
else if (process.argv.slice(-3).join(' ') === 'runtime descriptor --json') process.stdout.write(`${JSON.stringify(descriptor)}\n`);
else process.exit(1);
CLI
        printf '%s\n' "// $(git -C "$prefix" rev-parse HEAD)" >> "${prefix}/packages/cli/dist/index.js"
        chmod +x "${prefix}/packages/cli/dist/index.js"
        if [ "${WAIT_FOR_BUILD:-}" = '1' ]; then
            touch "${UPDATE_READY}"
            while [ ! -f "${UPDATE_RELEASE}" ]; do sleep 0.01; done
        fi
        ;;
    *)
        echo "unexpected npm args: ${args[*]}" >&2
        exit 2
        ;;
esac
NODE
chmod +x "${FAKE_BIN}/npm" "${FAKE_BIN}/node"

git init --bare --quiet "$REMOTE_REPO"
git clone --quiet "$REMOTE_REPO" "$SOURCE_WORK"
git -C "$SOURCE_WORK" config user.email smoke@example.com
git -C "$SOURCE_WORK" config user.name Smoke
printf '%s\n' '{"scripts":{"build":"node -e 0"}}' > "${SOURCE_WORK}/package.json"
printf '%s\n' '{"lockfileVersion":3,"packages":{}}' > "${SOURCE_WORK}/package-lock.json"
git -C "$SOURCE_WORK" add package.json package-lock.json
git -C "$SOURCE_WORK" commit --quiet -m 'initial wp-codebox fixture'
INITIAL_SHA="$(git -C "$SOURCE_WORK" rev-parse HEAD)"
git -C "$SOURCE_WORK" push --quiet origin HEAD:main

printf '%s\n' 'updated' > "${SOURCE_WORK}/fixture.txt"
printf '%s\n' 'incompatible' > "${SOURCE_WORK}/incompatible"
git -C "$SOURCE_WORK" add fixture.txt incompatible
git -C "$SOURCE_WORK" commit --quiet -m 'updated wp-codebox fixture'
UPDATED_SHA="$(git -C "$SOURCE_WORK" rev-parse HEAD)"
git -C "$SOURCE_WORK" tag fixture-ref
git -C "$SOURCE_WORK" push --quiet origin HEAD:main fixture-ref

git -C "$SOURCE_WORK" rm --quiet incompatible
printf '%s\n' 'compatible' > "${SOURCE_WORK}/fixture.txt"
printf '%s\n' 'minimum-compatible' > "${SOURCE_WORK}/minimum-compatible"
git -C "$SOURCE_WORK" add fixture.txt minimum-compatible
git -C "$SOURCE_WORK" commit --quiet -m 'compatible wp-codebox fixture'
COMPATIBLE_SHA="$(git -C "$SOURCE_WORK" rev-parse HEAD)"
git -C "$SOURCE_WORK" push --quiet origin HEAD:main

printf '%s\n' 'final' > "${SOURCE_WORK}/fixture.txt"
git -C "$SOURCE_WORK" rm --quiet minimum-compatible
git -C "$SOURCE_WORK" add fixture.txt
git -C "$SOURCE_WORK" commit --quiet -m 'final wp-codebox fixture'
FINAL_SHA="$(git -C "$SOURCE_WORK" rev-parse HEAD)"
git -C "$SOURCE_WORK" push --quiet origin HEAD:main

OUTPUT="$(PATH="${FAKE_BIN}:$PATH" "$SCRIPT" --source "$REMOTE_REPO" --ref "$INITIAL_SHA" --cache-dir "$CACHE_DIR" --npm "${FAKE_BIN}/npm")"
case "$OUTPUT" in
    *"WP Codebox cache SHA: ${INITIAL_SHA}"*) ;;
    *)
        echo "Expected initial SHA in output" >&2
        echo "$OUTPUT" >&2
        exit 1
        ;;
esac
[ -f "${CACHE_DIR}/npm-install-ran" ] || { echo "npm install marker missing" >&2; exit 1; }
[ -f "${CACHE_DIR}/node_modules/@automattic/wp-codebox-core/dist/index.js" ] || { echo "core package artifact missing" >&2; exit 1; }
[ -L "$CACHE_DIR" ] || { echo "Managed cache path is not a stable release pointer" >&2; exit 1; }
INITIAL_RELEASE="$(readlink "$CACHE_DIR")"
INITIAL_READER_CLI="$(cd "$INITIAL_RELEASE" && pwd -P)/packages/cli/dist/index.js"
INITIAL_CLI_SHA="$(shasum -a 256 "${CACHE_DIR}/packages/cli/dist/index.js" | awk '{print $1}')"
[ "$(git -C "$CACHE_DIR" rev-parse HEAD)" = "$INITIAL_SHA" ] || { echo "Initial exact SHA was not selected" >&2; exit 1; }
"${FAKE_BIN}/node" "${CACHE_DIR}/packages/cli/dist/index.js" --version | grep -q '0.23.4' || { echo "Current WP Codebox fixture was not admitted" >&2; exit 1; }

IDENTITY_BEFORE_UPDATE="$(cat "${CACHE_DIR}/.homeboy-runtime-identity.json")"
if OUTPUT="$(PATH="${FAKE_BIN}:$PATH" "$SCRIPT" --source "$REMOTE_REPO" --ref fixture-ref --cache-dir "$CACHE_DIR" --npm "${FAKE_BIN}/npm" 2>&1)"; then
    echo "Expected incompatible WP Codebox update to fail" >&2
    exit 1
fi
case "$OUTPUT" in
    *"wp_codebox_version_too_old: required >=0.21.0, observed 0.20.0"*) ;;
    *)
        echo "Incompatible update did not report an actionable version failure" >&2
        echo "$OUTPUT" >&2
        exit 1
        ;;
esac
[ "$(cat "${CACHE_DIR}/.homeboy-runtime-identity.json")" = "$IDENTITY_BEFORE_UPDATE" ] || { echo "Incompatible update replaced managed runtime identity" >&2; exit 1; }
[ "$(git -C "$CACHE_DIR" rev-parse HEAD)" = "$INITIAL_SHA" ] || { echo "Incompatible update replaced managed runtime HEAD" >&2; exit 1; }
[ "$(shasum -a 256 "${CACHE_DIR}/packages/cli/dist/index.js" | awk '{print $1}')" = "$INITIAL_CLI_SHA" ] || { echo "Incompatible update replaced managed CLI bytes" >&2; exit 1; }
"${FAKE_BIN}/node" "${CACHE_DIR}/packages/cli/dist/index.js" --version | grep -q '0.23.4' || { echo "Incompatible update replaced the current managed CLI" >&2; exit 1; }

# Hold a writer after it has acquired the update lock. Both resolver families
# must fail as updating rather than reach the stale PATH candidate.
STALE_PATH="${WORK_DIR}/stale-path"
STALE_MARKER="${WORK_DIR}/stale-path-invoked"
mkdir -p "$STALE_PATH"
cat > "${STALE_PATH}/wp-codebox" <<EOF
#!/usr/bin/env bash
touch "$STALE_MARKER"
exit 0
EOF
chmod +x "${STALE_PATH}/wp-codebox"
UPDATE_READY="${WORK_DIR}/update-ready"
UPDATE_RELEASE="${WORK_DIR}/update-release"
PROMOTION_READY="${WORK_DIR}/promotion-ready"
PROMOTION_RELEASE="${WORK_DIR}/promotion-release"
WAIT_FOR_BUILD=1 UPDATE_READY="$UPDATE_READY" UPDATE_RELEASE="$UPDATE_RELEASE" HOMEBOY_WP_CODEBOX_PROMOTION_READY_FILE="$PROMOTION_READY" HOMEBOY_WP_CODEBOX_PROMOTION_RELEASE_FILE="$PROMOTION_RELEASE" PATH="${FAKE_BIN}:$PATH" "$SCRIPT" --source "$REMOTE_REPO" --ref "$COMPATIBLE_SHA" --cache-dir "$CACHE_DIR" --npm "${FAKE_BIN}/npm" > "${WORK_DIR}/concurrent-update.out" 2>&1 &
UPDATE_PID=$!
for _ in $(seq 1 500); do [ -f "$UPDATE_READY" ] && break; sleep 0.01; done
[ -f "$UPDATE_READY" ] || { echo "Concurrent update did not reach the build barrier" >&2; exit 1; }
set +e
CONCURRENT_SHELL_OUTPUT="$(HOMEBOY_WP_CODEBOX_INSTALL_DIR="$(dirname "$CACHE_DIR")" PATH="${STALE_PATH}:$PATH" bash -c 'source "$1" && homeboy_wp_codebox_resolve_bin' wp-codebox-resolution "${SCRIPT_DIR}/../lib/wp-codebox-paths.sh" 2>&1)"
CONCURRENT_SHELL_STATUS=$?
set -e
[ "$CONCURRENT_SHELL_STATUS" -ne 0 ] && [[ "$CONCURRENT_SHELL_OUTPUT" == *"cache is updating"* ]] || { echo "Shell resolver did not fail as updating: $CONCURRENT_SHELL_OUTPUT" >&2; exit 1; }
node - "$(cd "${SCRIPT_DIR}/../.." && pwd)" "$(dirname "$CACHE_DIR")" "${STALE_PATH}" <<'NODE'
const path = require('node:path');
const { preflightWpCodeboxRuntime } = require(path.join(process.argv[2], '..', 'agent-runtimes/wp-codebox/lib/wp-codebox-runtime-selection.js'));
const result = preflightWpCodeboxRuntime({ env: { ...process.env, HOMEBOY_WP_CODEBOX_INSTALL_DIR: process.argv[3], PATH: `${process.argv[4]}:${process.env.PATH}` } });
if (result.reason !== 'wp_codebox_managed_updating') process.exit(1);
NODE
[ ! -e "$STALE_MARKER" ] || { echo "Concurrent reader executed stale PATH runtime" >&2; exit 1; }
touch "$UPDATE_RELEASE"
[ -f "$UPDATE_RELEASE" ] || { echo "Concurrent update did not leave build barrier" >&2; exit 1; }
for _ in $(seq 1 500); do [ -f "$PROMOTION_READY" ] && break; sleep 0.01; done
[ -f "$PROMOTION_READY" ] || { echo "Concurrent update did not reach the atomic promotion barrier" >&2; exit 1; }
# Resolve this path before the atomic pointer replacement, then execute it only
# after the new pointer is live. A removed old release would make this fail.
READER_CLI="$(cd "$(dirname "${CACHE_DIR}/packages/cli/dist/index.js")" && pwd -P)/index.js"
[ "$READER_CLI" = "$INITIAL_READER_CLI" ] || { echo "Reader did not resolve the old immutable release: expected $INITIAL_READER_CLI, got $READER_CLI" >&2; exit 1; }
touch "$PROMOTION_RELEASE"
wait "$UPDATE_PID"
"${FAKE_BIN}/node" "$READER_CLI" --version | grep -q '0.23.4' || { echo "Reader resolved before promotion could not execute afterward" >&2; exit 1; }

[ "$(git -C "$CACHE_DIR" rev-parse HEAD)" = "$COMPATIBLE_SHA" ] || { echo "Successful exact-SHA update did not replace managed HEAD" >&2; exit 1; }
COMPATIBLE_CLI_SHA="$(shasum -a 256 "${CACHE_DIR}/packages/cli/dist/index.js" | awk '{print $1}')"
[ "$COMPATIBLE_CLI_SHA" != "$INITIAL_CLI_SHA" ] || { echo "Successful exact-SHA update did not replace managed CLI bytes" >&2; exit 1; }
grep -q "\"source_sha\":\"${COMPATIBLE_SHA}\"" "${CACHE_DIR}/.homeboy-runtime-identity.json" || { echo "Successful exact-SHA update did not replace managed identity" >&2; exit 1; }
[ -e "$INITIAL_RELEASE" ] || { echo "Successful update removed the release held by a pre-promotion reader" >&2; exit 1; }
"${FAKE_BIN}/node" "${CACHE_DIR}/packages/cli/dist/index.js" --version | grep -q '0.21.0' || { echo "Minimum compatible WP Codebox fixture was not admitted" >&2; exit 1; }
"${FAKE_BIN}/node" "${CACHE_DIR}/packages/cli/dist/index.js" runtime descriptor --json | grep -q 'browser-contained-site-open' || { echo "Successful exact-SHA update left managed CLI unready" >&2; exit 1; }

# A reader may still hold A after two later atomic promotions. Retaining only
# the immediately previous release would reclaim A during this C promotion.
PATH="${FAKE_BIN}:$PATH" "$SCRIPT" --source "$REMOTE_REPO" --ref "$FINAL_SHA" --cache-dir "$CACHE_DIR" --npm "${FAKE_BIN}/npm" >/dev/null
[ "$(git -C "$CACHE_DIR" rev-parse HEAD)" = "$FINAL_SHA" ] || { echo "Final exact-SHA update did not replace managed HEAD" >&2; exit 1; }
[ -e "$INITIAL_RELEASE" ] || { echo "A release was reclaimed after A-to-B-to-C promotion" >&2; exit 1; }
"${FAKE_BIN}/node" "$READER_CLI" --version | grep -q '0.23.4' || { echo "A reader could not execute after A-to-B-to-C promotion" >&2; exit 1; }

cat > "${FAKE_BIN}/homeboy" <<'HOMEBOY'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$HOMEBOY_CALLS"
printf '%s\n' "$*"
cat
HOMEBOY
chmod +x "${FAKE_BIN}/homeboy"

HOMEBOY_CALLS="${WORK_DIR}/homeboy-calls"
LOCAL_DRY_RUN_OUTPUT="$(HOMEBOY_CALLS="$HOMEBOY_CALLS" PATH="${FAKE_BIN}:$PATH" "$SCRIPT" --source "$REMOTE_REPO" --ref fixture-ref --dry-run)"
if [ -e "$HOMEBOY_CALLS" ]; then
    echo "Local dry-run unexpectedly dispatched through Homeboy" >&2
    cat "$HOMEBOY_CALLS" >&2
    exit 1
fi

# Linux runners commonly expose sha256sum instead of macOS's shasum. The
# portable helper must retain that fallback.
if grep -q 'command -v shasum' "${SCRIPT}" && grep -q 'command -v sha256sum' "${SCRIPT}"; then
    :
else
    echo "Expected portable sha256 helper to support shasum and sha256sum" >&2
    exit 1
fi
case "$LOCAL_DRY_RUN_OUTPUT" in
    *"Fetching WP Codebox ref"*) ;;
    *)
        echo "Local dry-run output did not include cache update script" >&2
        echo "$LOCAL_DRY_RUN_OUTPUT" >&2
        exit 1
        ;;
esac
case "$LOCAL_DRY_RUN_OUTPUT" in
    *"runner exec"*)
        echo "Local dry-run emitted runner dispatch" >&2
        echo "$LOCAL_DRY_RUN_OUTPUT" >&2
        exit 1
        ;;
esac

DRY_RUN_OUTPUT="$(HOMEBOY_CALLS="$HOMEBOY_CALLS" PATH="${FAKE_BIN}:$PATH" "$SCRIPT" --runner example-runner --source "$REMOTE_REPO" --ref fixture-ref --dry-run)"
case "$DRY_RUN_OUTPUT" in
    "runner exec --script-file - --raw --env SOURCE="*) ;;
    *)
        echo "Dry-run did not pass runner exec options before runner id" >&2
        echo "$DRY_RUN_OUTPUT" >&2
        exit 1
        ;;
esac
case "$DRY_RUN_OUTPUT" in
    *"--env REQUESTED_REF=fixture-ref"*"--env CACHE_DIR="*"--env NPM_BIN=npm --dry-run example-runner"*) ;;
    *)
        echo "Dry-run did not preserve daemon-backed runner environment and target" >&2
        echo "$DRY_RUN_OUTPUT" >&2
        exit 1
        ;;
esac
case "$DRY_RUN_OUTPUT" in
    *"--ssh"*)
        echo "Dry-run unexpectedly requested diagnostic SSH" >&2
        echo "$DRY_RUN_OUTPUT" >&2
        exit 1
        ;;
esac
case "$DRY_RUN_OUTPUT" in
    *"Fetching WP Codebox ref"*) ;;
    *)
        echo "Dry-run output did not include cache update script" >&2
        echo "$DRY_RUN_OUTPUT" >&2
        exit 1
        ;;
esac

PATH_WITHOUT_FAKE_BIN="$(printf '%s' "$PATH" | tr ':' '\n' | grep -v -F "$FAKE_BIN" | paste -sd ':' -)"
OUTPUT="$(PATH="$PATH_WITHOUT_FAKE_BIN" "$SCRIPT" --source "$REMOTE_REPO" --ref "$INITIAL_SHA" --cache-dir "$CACHE_DIR" --npm "${FAKE_BIN}/npm")"
case "$OUTPUT" in
    *"WP Codebox cache SHA: ${INITIAL_SHA}"*) ;;
    *)
        echo "Expected absolute npm path run to succeed" >&2
        echo "$OUTPUT" >&2
        exit 1
        ;;
esac

echo "WP Codebox cache update smoke passed"
