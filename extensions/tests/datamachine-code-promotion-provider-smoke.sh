#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROVIDER="${ROOT_DIR}/scripts/datamachine-code-promotion-provider.sh"

if [ ! -f "$PROVIDER" ]; then
    echo "Missing provider script: $PROVIDER" >&2
    exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "jq is required for this smoke test" >&2
    exit 127
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

PATCH_FILE="${TMP_DIR}/changes.patch"
FAKE_BIN="${TMP_DIR}/bin"
STATE_DIR="${TMP_DIR}/state"
LOG_FILE="${TMP_DIR}/studio.log"
mkdir -p "$FAKE_BIN" "$STATE_DIR"

cat >"$PATCH_FILE" <<'PATCH'
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1 @@
-old
+new
PATCH

cat >"${FAKE_BIN}/studio" <<'STUDIO'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"${STUDIO_LOG:?}"

if [ "$1" != "wp" ] || [ "$2" != "datamachine-code" ] || [ "$3" != "workspace" ]; then
    echo "unexpected command prefix: $*" >&2
    exit 2
fi

case "$4 $5" in
    "worktree list")
        repo="$6"
        if [ -f "${STUDIO_STATE:?}/${repo}.added" ]; then
            jq -n --arg handle "${repo}@fix-3690" --arg path "/tmp/${repo}@fix-3690" '[{handle:$handle,path:$path}]'
        else
            printf '[]\n'
        fi
        ;;
    "worktree add")
        repo="$6"
        branch="$7"
        if [ "$repo" != "homeboy" ] || [ "$branch" != "fix-3690" ]; then
            echo "unexpected add target: $repo $branch" >&2
            exit 3
        fi
        touch "${STUDIO_STATE:?}/${repo}.added"
        printf '{"created":true}\n'
        ;;
    "patch apply")
        handle="$6"
        patch_arg="$7"
        format_arg="$8"
        if [ "$handle" != "homeboy@fix-3690" ]; then
            echo "unexpected patch handle: $handle" >&2
            exit 4
        fi
        case "$patch_arg" in
            --patch=@*) ;;
            *) echo "unexpected patch arg: $patch_arg" >&2; exit 5 ;;
        esac
        if [ "$format_arg" != "--format=json" ]; then
            echo "unexpected format arg: $format_arg" >&2
            exit 6
        fi
        printf '{"applied":true}\n'
        ;;
    *)
        echo "unexpected workspace operation: $4 $5" >&2
        exit 7
        ;;
esac
STUDIO
chmod +x "${FAKE_BIN}/studio"

REQUEST="$(jq -n \
    --arg patch_path "$PATCH_FILE" \
    '{
        schema:"homeboy/agent-task-promotion-apply-request/v1",
        to_workspace:"homeboy@fix-3690",
        patch_path:$patch_path,
        changed_files:["src/lib.rs"]
    }')"

RESPONSE="$(PATH="${FAKE_BIN}:$PATH" STUDIO_LOG="$LOG_FILE" STUDIO_STATE="$STATE_DIR" "$PROVIDER" <<<"$REQUEST")"

if ! jq -e '.schema == "homeboy/agent-task-promotion-apply-response/v1"' <<<"$RESPONSE" >/dev/null; then
    echo "Unexpected response schema: $RESPONSE" >&2
    exit 1
fi
if ! jq -e '.workspace_path == "/tmp/homeboy@fix-3690"' <<<"$RESPONSE" >/dev/null; then
    echo "Unexpected workspace path: $RESPONSE" >&2
    exit 1
fi
if ! jq -e '.command_evidence | length == 2' <<<"$RESPONSE" >/dev/null; then
    echo "Expected add and apply command evidence: $RESPONSE" >&2
    exit 1
fi
if ! grep -Fq 'wp datamachine-code workspace worktree list homeboy --format=json' "$LOG_FILE"; then
    echo "Expected worktree list command in log" >&2
    exit 1
fi
if ! grep -Fq 'wp datamachine-code workspace worktree add homeboy fix-3690' "$LOG_FILE"; then
    echo "Expected worktree add command in log" >&2
    exit 1
fi
if ! grep -Fq 'wp datamachine-code workspace patch apply homeboy@fix-3690' "$LOG_FILE"; then
    echo "Expected patch apply command in log" >&2
    exit 1
fi

printf 'ok\n'
