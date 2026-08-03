#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="${SCRIPT_DIR}/update-wp-codebox-cache.sh"
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
        mkdir -p "${prefix}/node_modules/@automattic/wp-codebox-core/dist"
        printf '%s\n' 'built' > "${prefix}/node_modules/@automattic/wp-codebox-core/dist/index.js"
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
git -C "$SOURCE_WORK" add fixture.txt
git -C "$SOURCE_WORK" commit --quiet -m 'updated wp-codebox fixture'
UPDATED_SHA="$(git -C "$SOURCE_WORK" rev-parse HEAD)"
git -C "$SOURCE_WORK" tag fixture-ref
git -C "$SOURCE_WORK" push --quiet origin HEAD:main fixture-ref

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

OUTPUT="$(PATH="${FAKE_BIN}:$PATH" "$SCRIPT" --source "$REMOTE_REPO" --ref fixture-ref --cache-dir "$CACHE_DIR" --npm "${FAKE_BIN}/npm")"
case "$OUTPUT" in
    *"WP Codebox cache SHA: ${UPDATED_SHA}"*) ;;
    *)
        echo "Expected updated SHA in output" >&2
        echo "$OUTPUT" >&2
        exit 1
        ;;
esac

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
    *"--env REQUESTED_REF=fixture-ref"*"--env CACHE_DIR="*"--env NPM_BIN=npm --ssh --dry-run example-runner"*) ;;
    *)
        echo "Dry-run did not preserve runner environment, SSH dispatch, and target" >&2
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
OUTPUT="$(PATH="$PATH_WITHOUT_FAKE_BIN" "$SCRIPT" --source "$REMOTE_REPO" --ref fixture-ref --cache-dir "$CACHE_DIR" --npm "${FAKE_BIN}/npm")"
case "$OUTPUT" in
    *"WP Codebox cache SHA: ${UPDATED_SHA}"*) ;;
    *)
        echo "Expected absolute npm path run to succeed" >&2
        echo "$OUTPUT" >&2
        exit 1
        ;;
esac

echo "WP Codebox cache update smoke passed"
