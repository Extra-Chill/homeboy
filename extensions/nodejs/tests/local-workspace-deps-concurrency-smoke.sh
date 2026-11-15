#!/usr/bin/env bash

if [ -z "${BASH_VERSION:-}" ]; then
    exec bash "$0" "$@"
fi

set -euo pipefail

# Concurrent consumers of one local workspace dependency (homeboy#18163).
#
# A monorepo deploy builds many components in parallel, and each consumer of
# a shared workspace package (e.g. @extrachill/tokens) applies the override.
# With a cold tarball cache, every consumer ran the dependency's install and
# build in the SAME source directory at the same time. `npm ci` deletes
# node_modules before reinstalling, so one consumer wiped typescript/lib out
# from under another consumer's running `tsc`:
#   error TS6053: File '.../packages/tokens/node_modules/typescript/lib/lib.es2020.d.ts' not found.
#
# The fixture build wipes its output, pauses, rewrites it, then verifies it was
# not disturbed. An overlapping build in the same directory breaks that check,
# and a lock that does not re-check the cache shows up as extra builds.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
HELPER="${EXTENSION_DIR}/../scripts/lib/local-workspace-deps.sh"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-lwd-concurrency.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

command -v npm >/dev/null 2>&1 || { echo "npm is required" >&2; exit 1; }

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

CONSUMERS=6
builds_log="${TMP_DIR}/builds.log"
: > "$builds_log"

make_source() {
    local root="$1" pkg="${1}/packages/shared"
    mkdir -p "$pkg"
    cat > "${pkg}/package.json" <<'JSON'
{
  "name": "@scope/shared",
  "version": "1.0.0",
  "main": "dist/index.js",
  "files": ["dist", "package.json"],
  "scripts": { "build": "node build.js" }
}
JSON
    # Exclusive-create a sentinel: a second build running in this directory at
    # the same time fails immediately instead of relying on timing alone.
    cat > "${pkg}/build.js" <<JS
const fs = require('fs');
fs.appendFileSync('${builds_log}', 'build\\n');
const sentinel = '.building';
try { fs.writeFileSync(sentinel, String(process.pid), { flag: 'wx' }); }
catch (e) { console.error('overlapping build in the same source directory'); process.exit(1); }
fs.rmSync('dist', { recursive: true, force: true });
Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 400);
fs.mkdirSync('dist', { recursive: true });
fs.writeFileSync('dist/index.js', 'module.exports = "built";\\n');
Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 200);
const ok = fs.existsSync('dist/index.js');
fs.rmSync(sentinel, { force: true });
if (!ok) { console.error('build output disappeared mid-build'); process.exit(1); }
JS
    printf 'dist/\nnode_modules/\n.building\n' > "${root}/.gitignore"
}

make_consumers() {
    local prefix="$1" i
    for i in $(seq 1 "$CONSUMERS"); do
        mkdir -p "${prefix}-${i}"
        printf '{ "name": "consumer-%s", "version": "1.0.0", "private": true }\n' "$i" > "${prefix}-${i}/package.json"
    done
}

run_parallel() {
    local source="$1" prefix="$2" settings i pids=() status=0
    export HOMEBOY_LOCAL_WORKSPACE_DEP_LOCK="${HOMEBOY_LOCAL_WORKSPACE_DEP_LOCK:-}"
    settings="$(node -e 'process.stdout.write(JSON.stringify({local_workspace_dependencies:[{name:"@scope/shared",path:process.argv[1],package_dir:"packages/shared"}]}))' "$source")"
    for i in $(seq 1 "$CONSUMERS"); do
        HOMEBOY_SETTINGS_JSON="$settings" \
            bash -c 'source "$1"; homeboy_apply_local_workspace_dependencies "$2"' \
            _ "$HELPER" "${prefix}-${i}" > "${prefix}-${i}.out" 2>&1 &
        pids+=("$!")
    done
    for i in "${!pids[@]}"; do
        if ! wait "${pids[$i]}"; then
            status=1
            echo "--- consumer $((i + 1)) output:" >&2
            sed 's/^/  /' "${prefix}-$((i + 1)).out" >&2
        fi
    done
    return "$status"
}

assert_installed() {
    local prefix="$1" i
    for i in $(seq 1 "$CONSUMERS"); do
        grep -Fq built "${prefix}-${i}/node_modules/@scope/shared/dist/index.js" 2>/dev/null \
            || fail "consumer ${i} did not get the built dependency"
    done
}

# --- Clean commit: parallel consumers share ONE build -------------------------
clean_src="${TMP_DIR}/clean-monorepo"
make_source "$clean_src"
git -C "$clean_src" init -q
git -C "$clean_src" add -A
git -C "$clean_src" -c user.name=t -c user.email=t@t commit -qm init
make_consumers "${TMP_DIR}/clean-consumer"

run_parallel "$clean_src" "${TMP_DIR}/clean-consumer" || fail "parallel consumers failed at a clean commit"
assert_installed "${TMP_DIR}/clean-consumer"
builds="$(grep -c build "$builds_log")"
[ "$builds" -eq 1 ] || fail "expected one shared build at a clean commit, got ${builds}"
echo "ok: ${CONSUMERS} parallel consumers at a clean commit share one build"

# --- No reusable cache (not a git repository): builds serialize ---------------
: > "$builds_log"
plain_src="${TMP_DIR}/plain-monorepo"
make_source "$plain_src"
make_consumers "${TMP_DIR}/plain-consumer"

run_parallel "$plain_src" "${TMP_DIR}/plain-consumer" || fail "parallel consumers failed without a reusable cache"
assert_installed "${TMP_DIR}/plain-consumer"
builds="$(grep -c build "$builds_log")"
[ "$builds" -eq "$CONSUMERS" ] || fail "expected ${CONSUMERS} serialized builds without a cache, got ${builds}"
echo "ok: ${CONSUMERS} parallel consumers without a cache serialize their builds"

# --- The package's own component build overlaps its consumers -----------------
# A workspace package is often also a component (extrachill-tokens), so the
# nodejs build-runner installs and builds it in the same directory while the
# consumers build it as an override. The runner holds the same lock.
: > "$builds_log"
comp_src="${TMP_DIR}/component-monorepo"
make_source "$comp_src"
make_consumers "${TMP_DIR}/component-consumer"
component_build() {
    bash -c 'source "$1"; homeboy_local_workspace_lock_acquire "$2" h || exit 1; ( cd "$2" && node build.js ); rc=$?; homeboy_local_workspace_lock_release h; exit $rc' \
        _ "$HELPER" "${comp_src}/packages/shared"
}
component_build > "${TMP_DIR}/component.out" 2>&1 &
component_pid=$!
# Start the consumers only once the component build is mid-build, so a missing
# lock collides deterministically instead of depending on scheduling.
for _ in $(seq 1 100); do [ -e "${comp_src}/packages/shared/.building" ] && break; sleep 0.05; done
[ -e "${comp_src}/packages/shared/.building" ] || fail "component build never started"
run_parallel "$comp_src" "${TMP_DIR}/component-consumer" || fail "consumers collided with the package's own component build"
wait "$component_pid" || { sed 's/^/  /' "${TMP_DIR}/component.out" >&2; fail "component build collided with its consumers"; }
assert_installed "${TMP_DIR}/component-consumer"
echo "ok: the package's own component build and its consumers never overlap"

# Locks on different directories nest (component dir, then dependency dir).
nest_a="${TMP_DIR}/nest-a"; nest_b="${TMP_DIR}/nest-b"; mkdir -p "$nest_a" "$nest_b"
bash -c 'source "$1"; homeboy_local_workspace_lock_acquire "$2" a && homeboy_local_workspace_lock_acquire "$3" b \
    && homeboy_local_workspace_lock_release b && homeboy_local_workspace_lock_release a \
    && HOMEBOY_LOCAL_WORKSPACE_DEP_LOCK_TIMEOUT=1 homeboy_local_workspace_lock_acquire "$2" c' \
    _ "$HELPER" "$nest_a" "$nest_b" || fail "nested locks on different directories failed"
echo "ok: locks on different directories nest and release"

# --- A stale lock from a dead process does not wedge builds -------------------
: > "$builds_log"
stale_src="${TMP_DIR}/stale-monorepo"
make_source "$stale_src"
make_consumers "${TMP_DIR}/stale-consumer"
lock_root="${TMPDIR:-/tmp}/homeboy-local-workspace-deps-locks"
mkdir -p "$lock_root"
stale_key="$(printf '%s' "$(cd "${stale_src}" && pwd -P)" | { command -v sha256sum >/dev/null 2>&1 && sha256sum || shasum -a 256; } | awk '{print $1}')"
mkdir -p "${lock_root}/${stale_key}.lock"
printf '%s' 999999999 > "${lock_root}/${stale_key}.lock/pid"
HOMEBOY_LOCAL_WORKSPACE_DEP_LOCK=mkdir run_parallel "$stale_src" "${TMP_DIR}/stale-consumer" \
    || fail "a stale lock wedged the build"
assert_installed "${TMP_DIR}/stale-consumer"
[ ! -d "${lock_root}/${stale_key}.lock" ] || fail "mkdir lock left behind"
builds="$(grep -c build "$builds_log")"
[ "$builds" -eq "$CONSUMERS" ] || fail "expected ${CONSUMERS} serialized mkdir-locked builds, got ${builds}"
echo "ok: mkdir fallback serializes builds and reclaims a lock left by a dead process"

echo "local workspace deps concurrency smoke passed"
