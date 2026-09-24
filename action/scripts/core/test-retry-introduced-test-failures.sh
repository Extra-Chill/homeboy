#!/usr/bin/env bash

# Extra-Chill/homeboy#14984: exercise retry-introduced-test-failures.sh
# directly, against real cargo/cargo-nextest plugin dispatch and fake test
# runner plugins, so its retry-loop and sidecar-writing logic is proven
# without needing a live GitHub Actions checkout.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RETRY_SCRIPT="${SCRIPT_DIR}/retry-introduced-test-failures.sh"

assert_equals() {
  local expected="$1" actual="$2" label="$3"
  if [ "${expected}" != "${actual}" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual:   %s\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi
  printf 'PASS: %s\n' "${label}"
}

tmp="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-retry-introduced.XXXXXX")"
trap 'rm -rf "${tmp}"' EXIT

current_dir="${tmp}/current"
base_dir="${tmp}/base"
workspace="${tmp}/workspace"
bin_dir="${tmp}/bin"
mkdir -p "${current_dir}" "${base_dir}" "${workspace}" "${bin_dir}"

compute_inventory_fingerprint() {
  python3 - "$@" <<'PY'
import json
import sys
from hashlib import sha256

command, runner = sys.argv[1], sys.argv[2]
ids = sys.argv[3:]
canonical = {
    "command": command,
    "execution_fingerprint": "c" * 64,
    "runner": runner,
    "runner_fingerprint": "a" * 64,
    "schema": "homeboy/test-inventory/v1",
    "tests": [{"id": identity} for identity in sorted(ids)],
    "workspace_fingerprint": "b" * 64,
}
print(sha256(json.dumps(canonical, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest())
PY
}

write_outcomes() {
  # write_outcomes DIR COMMAND RUNNER FAILED_ID... -- ALL_ID...
  local dir="$1" command="$2" runner="$3"
  shift 3
  local failed_ids=() all_ids=() collecting_failed=true
  for arg in "$@"; do
    if [ "${arg}" = "--" ]; then
      collecting_failed=false
      continue
    fi
    if [ "${collecting_failed}" = true ]; then
      failed_ids+=("${arg}")
    else
      all_ids+=("${arg}")
    fi
  done
  local fp stem
  fp="$(compute_inventory_fingerprint "${command}" "${runner}" "${all_ids[@]}")"
  stem="$(printf '%s' "${command}" | sed -E 's/[^[:alnum:]._-]+/-/g; s/^-+//; s/-+$//')"
  jq -cn --arg command "${command}" --arg runner "${runner}" --arg fp "${fp}" \
    --argjson failed "$(printf '%s\n' "${failed_ids[@]:-}" | jq -R 'select(length > 0)' | jq -sc .)" \
    '{schema:"homeboy/test-outcomes/v1",command:$command,runner:$runner,runner_fingerprint:("a"*64),workspace_fingerprint:("b"*64),execution_fingerprint:("c"*64),inventory_fingerprint:$fp,failed_test_ids:$failed}' \
    > "${dir}/${stem}.test-outcomes.json"
  jq -cn --arg command "${command}" --arg runner "${runner}" --arg fp "${fp}" \
    --argjson tests "$(printf '%s\n' "${all_ids[@]}" | jq -R '{id:.}' | jq -sc .)" \
    '{schema:"homeboy/test-inventory/v1",command:$command,runner:$runner,runner_fingerprint:("a"*64),workspace_fingerprint:("b"*64),execution_fingerprint:("c"*64),inventory_fingerprint:$fp,tests:$tests}' \
    > "${dir}/${stem}.test-inventory.json"
}

reset_fixtures() {
  rm -rf "${current_dir}" "${base_dir}"
  mkdir -p "${current_dir}" "${base_dir}"
}

run_retry() {
  local extra_path="$1"
  shift
  COMMAND='review test' CURRENT_DIR="${current_dir}" BASE_DIR="${base_dir}" WORKSPACE="${workspace}" \
    PATH="${extra_path}:${PATH}" \
    bash "${RETRY_SCRIPT}" "$@"
}

sidecar() {
  jq -c "$1" "${current_dir}/review-test.test-retry.json"
}

# --- Nothing to retry --------------------------------------------------------
reset_fixtures
write_outcomes "${current_dir}" 'review test' nextest -- id_a
write_outcomes "${base_dir}" 'review test' nextest id_a -- id_a
output="$(run_retry "${bin_dir}" 2>&1)"
[ ! -f "${current_dir}/review-test.test-retry.json" ] || { printf 'FAIL: a sidecar was written when there was nothing to retry\n'; exit 1; }
case "${output}" in
  *"nothing to retry"*) printf 'PASS: no introduced failures writes no sidecar and says so\n' ;;
  *) printf 'FAIL: missing "nothing to retry" message\n%s\n' "${output}"; exit 1 ;;
esac

# --- Unsupported runner -------------------------------------------------------
reset_fixtures
write_outcomes "${current_dir}" 'review test' phpunit id_a -- id_a id_b
write_outcomes "${base_dir}" 'review test' phpunit -- id_b
output="$(run_retry "${bin_dir}" 2>&1)"
case "${output}" in
  *"::notice::"*"no supported retry runner"*"phpunit"*) printf 'PASS: an unsupported runner is announced by name\n' ;;
  *) printf 'FAIL: unsupported runner was not announced\n%s\n' "${output}"; exit 1 ;;
esac
assert_equals '{"flaky":[],"still_failing":["id_a"],"unsupported_runner":"phpunit"}' \
  "$(sidecar '{flaky,still_failing,unsupported_runner}')" \
  "an unsupported runner leaves every identity still_failing and names itself"

# --- MAX_RETRIES validation ---------------------------------------------------
reset_fixtures
write_outcomes "${current_dir}" 'review test' nextest id_a -- id_a
write_outcomes "${base_dir}" 'review test' nextest -- id_a
set +e
output="$(COMMAND='review test' CURRENT_DIR="${current_dir}" BASE_DIR="${base_dir}" WORKSPACE="${workspace}" MAX_RETRIES=nope bash "${RETRY_SCRIPT}" 2>&1)"
status=$?
set -e
[ "${status}" -ne 0 ] || { printf 'FAIL: a non-numeric MAX_RETRIES did not fail\n'; exit 1; }
case "${output}" in
  *"MAX_RETRIES must be a non-negative integer"*) printf 'PASS: a non-numeric MAX_RETRIES is rejected\n' ;;
  *) printf 'FAIL: bad MAX_RETRIES message missing\n%s\n' "${output}"; exit 1 ;;
esac

# --- nextest: flaky passes within budget, one never does ---------------------
# The fake cargo-nextest plugin below is exercised through real cargo plugin
# dispatch (`cargo nextest ...` finds `cargo-nextest` on PATH), the same
# mechanism a real nextest install uses.
fake_nextest_dir="${tmp}/fake-nextest"
mkdir -p "${fake_nextest_dir}"
state_dir="${tmp}/state"
mkdir -p "${state_dir}"
cat > "${fake_nextest_dir}/cargo-nextest" <<'FAKE'
#!/usr/bin/env bash
id=""
for arg in "$@"; do
  case "${arg}" in
    test\(=*\)) id="${arg#test(=}"; id="${id%)}" ;;
  esac
done
case "${id}" in
  id_flaky)
    state="${RETRY_TEST_STATE_DIR}/id_flaky.count"
    count=0
    [ -f "${state}" ] && count="$(cat "${state}")"
    count=$((count + 1))
    echo "${count}" > "${state}"
    if [ "${count}" -ge 2 ]; then
      echo "1 tests run: 1 passed"
      exit 0
    fi
    echo "1 tests run: 0 passed, 1 failed"
    exit 100
    ;;
  id_stuck)
    echo "1 tests run: 0 passed, 1 failed"
    exit 100
    ;;
  *)
    echo "0 tests run"
    exit 4
    ;;
esac
FAKE
chmod +x "${fake_nextest_dir}/cargo-nextest"

reset_fixtures
rm -f "${state_dir}"/*.count
write_outcomes "${current_dir}" 'review test' nextest id_flaky id_stuck -- id_flaky id_stuck id_other
write_outcomes "${base_dir}" 'review test' nextest -- id_other
RETRY_TEST_STATE_DIR="${state_dir}" run_retry "${fake_nextest_dir}" MAX_RETRIES=2 >/dev/null 2>&1 || true
output="$(RETRY_TEST_STATE_DIR="${state_dir}" COMMAND='review test' CURRENT_DIR="${current_dir}" BASE_DIR="${base_dir}" WORKSPACE="${workspace}" PATH="${fake_nextest_dir}:${PATH}" MAX_RETRIES=2 bash "${RETRY_SCRIPT}" 2>&1)"
assert_equals '{"flaky":["id_flaky"],"still_failing":["id_stuck"]}' \
  "$(sidecar '{flaky,still_failing}')" \
  "a flaky nextest identity is excluded and a consistently-failing one is not"
case "${output}" in
  *"::warning::"*"flaky"*"id_flaky"*) printf 'PASS: the flaky identity is announced by name\n' ;;
  *) printf 'FAIL: flaky announcement missing id_flaky\n%s\n' "${output}"; exit 1 ;;
esac
case "${output}" in
  *"::warning::"*"remain introduced"*"id_stuck"*) printf 'PASS: the still-failing identity is announced by name\n' ;;
  *) printf 'FAIL: still-failing announcement missing id_stuck\n%s\n' "${output}"; exit 1 ;;
esac

# --- nextest: every identity fails every attempt -> none excluded -----------
reset_fixtures
rm -f "${state_dir}"/*.count
write_outcomes "${current_dir}" 'review test' nextest id_stuck -- id_stuck id_other
write_outcomes "${base_dir}" 'review test' nextest -- id_other
RETRY_TEST_STATE_DIR="${state_dir}" run_retry "${fake_nextest_dir}" MAX_RETRIES=2 >/dev/null 2>&1 || true
assert_equals '{"flaky":[],"still_failing":["id_stuck"]}' \
  "$(sidecar '{flaky,still_failing}')" \
  "a consistently failing identity is never classified flaky"

# --- cargo test fallback: real cargo, no nextest on PATH ---------------------
# Uses the real system cargo against a minimal on-disk crate so the exact-
# match filter and pass/fail detection are proven against real output, not a
# guess about its format.
mkdir -p "${workspace}/src"
cat > "${workspace}/Cargo.toml" <<'EOF'
[package]
name = "homeboy-retry-fixture"
version = "0.0.0"
edition = "2021"
EOF
cat > "${workspace}/src/lib.rs" <<'EOF'
#[cfg(test)]
mod tests {
    #[test]
    fn ok() {
        assert_eq!(1 + 1, 2);
    }

    #[test]
    fn bad() {
        assert_eq!(1 + 1, 3);
    }
}
EOF

reset_fixtures
write_outcomes "${current_dir}" 'review test' 'cargo test' tests::ok tests::bad -- tests::ok tests::bad tests::other
write_outcomes "${base_dir}" 'review test' 'cargo test' -- tests::other
output="$(run_retry "${bin_dir}" MAX_RETRIES=1 2>&1)"
assert_equals '{"flaky":["tests::ok"],"still_failing":["tests::bad"]}' \
  "$(sidecar '{flaky,still_failing}')" \
  "cargo test fallback correctly classifies a real passing test as flaky and a real failing test as introduced"
case "${output}" in
  *"tests::ok"*) printf 'PASS: cargo test path announces the flaky identity\n' ;;
  *) printf 'FAIL: cargo test path did not announce tests::ok\n%s\n' "${output}"; exit 1 ;;
esac

printf 'All retry-introduced-test-failures checks passed.\n'
