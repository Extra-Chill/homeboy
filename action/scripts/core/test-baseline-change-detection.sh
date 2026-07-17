#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/lib.sh"

assert_true() {
  local label="$1"
  shift

  if ! "$@" >/dev/null 2>&1; then
    printf 'FAIL: %s\n' "${label}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_false() {
  local label="$1"
  shift

  if "$@" >/dev/null 2>&1; then
    printf 'FAIL: %s\n' "${label}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_equals() {
  local expected="$1"
  local actual="$2"
  local label="$3"

  if [ "${expected}" != "${actual}" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual:   %s\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

# ── Fake homeboy ──
# drift_file_paths shells out to `homeboy component show` and parses the
# `data.entity.drift_files` array. Tests inject a fake binary so we can
# control the resolver output deterministically without depending on a
# specific homeboy version, extension layout, or registry state.
FAKE_BIN="$(mktemp -d)"
trap 'rm -rf "${FAKE_BIN}" "${REPO:-}" "${NON_RUST_REPO:-}" "${LEGACY_REPO:-}" "${ABSENT_REPO:-}"' EXIT

cat > "${FAKE_BIN}/homeboy" <<'SH'
#!/usr/bin/env bash
# Reads `${HOMEBOY_FAKE_RESPONSE}` (a path to a JSON file) and emits it
# verbatim. When the variable is unset, exits non-zero so drift_file_paths
# falls back to the audit-baseline-only legacy path.
if [ -z "${HOMEBOY_FAKE_RESPONSE:-}" ]; then
  exit 1
fi
cat "${HOMEBOY_FAKE_RESPONSE}"
SH
chmod +x "${FAKE_BIN}/homeboy"
export PATH="${FAKE_BIN}:${PATH}"

write_response() {
  local file="$1"
  shift
  local items=()
  for item in "$@"; do
    items+=("\"${item}\"")
  done
  local joined
  joined="$(IFS=','; echo "${items[*]}")"
  cat > "${file}" <<JSON
{ "data": { "entity": { "drift_files": [${joined}] } } }
JSON
}

# ── Repo fixtures ──
REPO="$(mktemp -d)"
git -C "${REPO}" init -q
git -C "${REPO}" config user.name test
git -C "${REPO}" config user.email test@example.com
printf '{"id":"root"}\n' > "${REPO}/homeboy.json"
printf '# placeholder lockfile\n' > "${REPO}/Cargo.lock"
mkdir -p "${REPO}/packages/plugin"
printf '{"id":"plugin"}\n' > "${REPO}/packages/plugin/homeboy.json"
printf 'initial\n' > "${REPO}/README.md"
git -C "${REPO}" add .
git -C "${REPO}" commit -q -m initial

cd "${REPO}"

# Simulate a Rust component: homeboy core resolves drift to
# `homeboy.json + Cargo.lock`.
RUST_RESPONSE="$(mktemp)"
write_response "${RUST_RESPONSE}" "homeboy.json" "Cargo.lock"
export HOMEBOY_FAKE_RESPONSE="${RUST_RESPONSE}"

# baseline_file_path stays workspace-relative (legacy contract).
assert_equals "homeboy.json" "$(baseline_file_path "${REPO}")" "root baseline path"
assert_equals "packages/plugin/homeboy.json" "$(baseline_file_path "${REPO}/packages/plugin")" "component baseline path"

# drift_file_paths returns repo-root-relative paths from homeboy's resolver.
expected_drift="$(printf 'homeboy.json\nCargo.lock\n')"
assert_equals "${expected_drift}" "$(drift_file_paths "${REPO}")" "root drift list (Rust component)"

# A sub-component workspace gets its prefix stitched onto every drift path.
expected_drift_component="$(printf 'packages/plugin/homeboy.json\npackages/plugin/Cargo.lock\n')"
assert_equals "${expected_drift_component}" "$(drift_file_paths "${REPO}/packages/plugin")" "sub-component drift list keeps prefix"

# homeboy.json-only diff is drift-only.
printf '{"id":"root","audit":{"baseline":[]}}\n' > homeboy.json
assert_true "root homeboy.json-only diff is drift-only" changes_are_only_drift "${REPO}"
assert_true "legacy alias accepts homeboy.json-only diff" changes_are_only_audit_baseline "${REPO}"

# Cargo.lock-only diff is drift-only when the resolver lists Cargo.lock.
git checkout -q -- homeboy.json
printf '# bumped lockfile\n' > Cargo.lock
assert_true "Cargo.lock-only diff is drift-only" changes_are_only_drift "${REPO}"

# Both files together is still drift-only.
printf '{"id":"root","audit":{"baseline":[]}}\n' > homeboy.json
assert_true "homeboy.json + Cargo.lock diff is drift-only" changes_are_only_drift "${REPO}"

# Source change alongside drift breaks the drift-only invariant.
printf 'changed\n' > README.md
assert_false "source plus drift diff is not drift-only" changes_are_only_drift "${REPO}"

git checkout -q -- README.md homeboy.json Cargo.lock

# ── Non-Rust repo: resolver omits Cargo.lock ──
# WordPress component fixture: composer.lock is in the resolver list, but
# this repo doesn't have one on disk. drift_file_paths returns the resolver
# list verbatim — existence checking lives in push/restore helpers.
NON_RUST_REPO="$(mktemp -d)"
git -C "${NON_RUST_REPO}" init -q
git -C "${NON_RUST_REPO}" config user.name test
git -C "${NON_RUST_REPO}" config user.email test@example.com
printf '{"id":"php"}\n' > "${NON_RUST_REPO}/homeboy.json"
printf 'README\n' > "${NON_RUST_REPO}/README.md"
git -C "${NON_RUST_REPO}" add .
git -C "${NON_RUST_REPO}" commit -q -m initial

WP_RESPONSE="$(mktemp)"
write_response "${WP_RESPONSE}" "homeboy.json" "composer.lock"
export HOMEBOY_FAKE_RESPONSE="${WP_RESPONSE}"
expected_wp="$(printf 'homeboy.json\ncomposer.lock\n')"
assert_equals "${expected_wp}" "$(drift_file_paths "${NON_RUST_REPO}")" "non-Rust repo follows resolver list"

# ── Legacy homeboy fallback ──
# When `homeboy component show` returns nothing parseable (older homeboy
# without drift_files, or homeboy missing entirely), fall back to the
# audit-baseline-only behavior shipped pre-#209.
LEGACY_REPO="$(mktemp -d)"
git -C "${LEGACY_REPO}" init -q
git -C "${LEGACY_REPO}" config user.name test
git -C "${LEGACY_REPO}" config user.email test@example.com
printf '{"id":"legacy"}\n' > "${LEGACY_REPO}/homeboy.json"
git -C "${LEGACY_REPO}" add .
git -C "${LEGACY_REPO}" commit -q -m initial

LEGACY_RESPONSE="$(mktemp)"
cat > "${LEGACY_RESPONSE}" <<'JSON'
{ "data": { "entity": { "id": "legacy" } } }
JSON
export HOMEBOY_FAKE_RESPONSE="${LEGACY_RESPONSE}"
assert_equals "homeboy.json" "$(drift_file_paths "${LEGACY_REPO}")" "legacy homeboy without drift_files falls back to baseline only"

unset HOMEBOY_FAKE_RESPONSE
assert_equals "homeboy.json" "$(drift_file_paths "${LEGACY_REPO}")" "homeboy show failure falls back to baseline only"

# ── Absent homeboy on PATH ──
# Stash the fake binary to simulate a host without homeboy. drift_file_paths
# must still return the audit baseline so the action is usable in
# bootstrap-style runs.
ABSENT_REPO="$(mktemp -d)"
git -C "${ABSENT_REPO}" init -q
git -C "${ABSENT_REPO}" config user.name test
git -C "${ABSENT_REPO}" config user.email test@example.com
printf '{"id":"absent"}\n' > "${ABSENT_REPO}/homeboy.json"
git -C "${ABSENT_REPO}" add .
git -C "${ABSENT_REPO}" commit -q -m initial

mv "${FAKE_BIN}/homeboy" "${FAKE_BIN}/homeboy.bak"
original_path="${PATH}"
# Removing the fake alone exposes a host-installed Homeboy, defeating this
# fixture and allowing its real component resolver to block the shell suite.
PATH="${FAKE_BIN}:/usr/bin:/bin"
assert_equals "homeboy.json" "$(drift_file_paths "${ABSENT_REPO}")" "no homeboy on PATH falls back to baseline only"
PATH="${original_path}"
mv "${FAKE_BIN}/homeboy.bak" "${FAKE_BIN}/homeboy"

printf 'All baseline change detection checks passed.\n'
