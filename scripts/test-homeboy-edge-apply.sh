#!/usr/bin/env bash
#
# Behaviour tests for scripts/homeboy-edge-apply.
#
# Run on Linux (the wrapper targets GNU stat/install/sha256sum):
#
#     scripts/test-homeboy-edge-apply.sh
#
# The wrapper hardcodes its config and state directories on purpose — they must
# not be overridable by the caller, because the caller is an unprivileged agent.
# To test it, we copy the script and rewrite those two constants, asserting that
# each rewrite actually matched so the test can never silently drift into
# exercising nothing.
#
# The REPO_URL scheme allowlist is asserted against the UNMODIFIED script, since
# relaxing it is exactly what those assertions exist to prevent.
#
set -Eeuo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
readonly REAL_SCRIPT="$HERE/homeboy-edge-apply"
[[ -f $REAL_SCRIPT ]] || { echo "cannot find homeboy-edge-apply" >&2; exit 1; }

ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT

readonly CONF_DIR="$ROOT/etc/homeboy-edge.d"
readonly STATE="$ROOT/var/lib/homeboy-edge"
readonly SCRIPT="$ROOT/homeboy-edge-apply"
mkdir -p "$CONF_DIR" "$ROOT/dest" "$ROOT/marker"

# Replace the single line containing a fixed marker, asserting it matched
# exactly once so the test can never drift into exercising a stale copy.
rewrite_line() {
    python3 - "$SCRIPT" "$1" "$2" <<'PY'
import sys
path, marker, replacement = sys.argv[1], sys.argv[2], sys.argv[3]
lines = open(path).read().splitlines(keepends=True)
hits = [i for i, l in enumerate(lines) if marker in l]
if len(hits) != 1:
    sys.exit("FATAL: test rewrite marker matched %d lines: %s" % (len(hits), marker))
lines[hits[0]] = replacement + "\n"
open(path, "w").writelines(lines)
PY
}

cp "$REAL_SCRIPT" "$SCRIPT"
rewrite_line 'readonly CONFIG_DIR=' "readonly CONFIG_DIR=$CONF_DIR"
rewrite_line 'readonly STATE_DIR=' "readonly STATE_DIR=$STATE"
# Local git fixtures cannot be served over https; the scheme allowlist itself is
# asserted separately against $REAL_SCRIPT (see "rejects non-https remotes").
rewrite_line 'must be a plain https URL' \
    '[[ $REPO_URL =~ ^https://[A-Za-z0-9._~/-]+$ || $REPO_URL =~ ^file:///[A-Za-z0-9._/-]+$ ]] || die 2 "REPO_URL must be a plain https URL"'
chmod 0755 "$SCRIPT"

PASS=0
FAIL=0
check() {
    local name=$1 expected=$2 actual=$3
    if [[ $expected == "$actual" ]]; then
        printf 'ok   %s\n' "$name"
        PASS=$((PASS + 1))
    else
        printf 'FAIL %s\n       expected: %s\n       actual:   %s\n' "$name" "$expected" "$actual"
        FAIL=$((FAIL + 1))
    fi
}

run() { set +e; "$SCRIPT" "$@" >"$ROOT/out" 2>&1; local rc=$?; set -e; printf '%s' "$rc"; }

# ---------------------------------------------------------------------------
# Fixture repository
# ---------------------------------------------------------------------------

readonly REPO="$ROOT/repo"
mkdir -p "$REPO/deploy"
git init --quiet --initial-branch=main "$REPO"
git -C "$REPO" config user.email test@example.invalid
git -C "$REPO" config user.name test
printf 'server { listen 80; }\n' > "$REPO/deploy/site.conf"
git -C "$REPO" add -A
git -C "$REPO" commit --quiet -m "initial"
MERGED_SHA=$(git -C "$REPO" rev-parse HEAD)

# A commit that exists on a branch but was never merged into main.
git -C "$REPO" checkout --quiet -b unmerged
printf 'server { listen 80; root /; }\n' > "$REPO/deploy/site.conf"
git -C "$REPO" add -A
git -C "$REPO" commit --quiet -m "not reviewed"
UNMERGED_SHA=$(git -C "$REPO" rev-parse HEAD)
git -C "$REPO" checkout --quiet main

readonly DEST="$ROOT/dest/site.conf"
readonly RELOADS="$ROOT/marker/reloads"
: > "$RELOADS"

cat > "$ROOT/marker/reload" <<EOF
#!/bin/sh
echo reloaded >> $RELOADS
EOF
chmod 0755 "$ROOT/marker/reload"

write_conf() {
    local validate=${1:-/bin/true} probe=${2:-}
    {
        printf 'REPO_URL=file://%s\n' "$REPO"
        printf 'SOURCE_PATH=deploy/site.conf\n'
        printf 'DEST_PATH=%s\n' "$DEST"
        printf 'VALIDATE_CMD=%s\n' "$validate"
        printf 'RELOAD_CMD=%s\n' "$ROOT/marker/reload"
        [[ -n $probe ]] && printf 'PROBE=%s\n' "$probe"
    } > "$CONF_DIR/site.conf"
    chmod 0644 "$CONF_DIR/site.conf"
}

reload_count() { wc -l < "$RELOADS" | tr -d ' '; }

# ---------------------------------------------------------------------------
# Input and configuration handling
# ---------------------------------------------------------------------------

check "no arguments is a usage error" 2 "$(run)"
check "invalid target name refused" 2 "$(run 'Bad/Target')"
check "unknown target refused" 2 "$(run nosuchtarget)"
check "short sha refused" 2 "$(run site deadbeef)"

write_conf
printf 'UNEXPECTED_KEY=1\n' >> "$CONF_DIR/site.conf"
check "unknown config key fails closed" 2 "$(run site)"

write_conf
chmod 0666 "$CONF_DIR/site.conf"
check "world-writable config refused" 2 "$(run site)"

# Against the real, unmodified script: a remote helper URL is command execution.
write_conf
sed -i "s|^REPO_URL=.*|REPO_URL=ext::sh -c touch% /tmp/pwned|" "$CONF_DIR/site.conf"
set +e
CONFIG_DIR_OVERRIDE_IGNORED=$("$REAL_SCRIPT" site 2>&1); rc=$?
set -e
check "rejects non-https remotes" 2 "$rc"

# ---------------------------------------------------------------------------
# Applying reviewed content
# ---------------------------------------------------------------------------

write_conf
check "applies tip of the trust ref" 0 "$(run site)"
check "dest matches committed content" "server { listen 80; }" "$(cat "$DEST")"
check "reload ran once" 1 "$(reload_count)"

check "re-running is a no-op" 0 "$(run site)"
check "no-op does not reload" 1 "$(reload_count)"

check "explicit merged sha applies" 0 "$(run site "$MERGED_SHA")"

# ---------------------------------------------------------------------------
# THE security property: content comes from the object store, never a checkout
# ---------------------------------------------------------------------------

# Simulate the agent writing hostile content into every checkout it can reach,
# including the fixture repo's own working tree, then invoking the wrapper.
printf 'server { root /; autoindex on; }\n' > "$REPO/deploy/site.conf"
check "tampered working tree ignored" 0 "$(run site)"
check "dest still matches the merged commit" "server { listen 80; }" "$(cat "$DEST")"
git -C "$REPO" checkout --quiet -- deploy/site.conf

check "unmerged commit refused" 4 "$(run site "$UNMERGED_SHA")"
check "dest unchanged after refusal" "server { listen 80; }" "$(cat "$DEST")"

# ---------------------------------------------------------------------------
# Drift
# ---------------------------------------------------------------------------

printf '# hand edited by an operator at 3am\n' >> "$DEST"
check "hand edit blocks apply" 3 "$(run site)"
check "hand edit preserved" "# hand edited by an operator at 3am" "$(tail -n1 "$DEST")"
check "--adopt records live state" 0 "$(run site --adopt)"
check "apply resumes after adopt" 0 "$(run site)"
check "dest back to committed content" "server { listen 80; }" "$(cat "$DEST")"

# ---------------------------------------------------------------------------
# Rollback
# ---------------------------------------------------------------------------

git -C "$REPO" checkout --quiet main
printf 'server { listen 8080; }\n' > "$REPO/deploy/site.conf"
git -C "$REPO" add -A
git -C "$REPO" commit --quiet -m "change that fails validation"

before_reloads=$(reload_count)
write_conf /bin/false
check "validation failure exits 1" 1 "$(run site)"
check "dest restored after failed validation" "server { listen 80; }" "$(cat "$DEST")"

write_conf /bin/true 'http://127.0.0.1:1/ 200'
check "probe failure exits 1" 1 "$(run site)"
check "dest restored after failed probe" "server { listen 80; }" "$(cat "$DEST")"

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ $FAIL -eq 0 ]]
