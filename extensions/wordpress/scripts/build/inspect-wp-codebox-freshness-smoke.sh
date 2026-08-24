#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

CLI="${TMPDIR}/wp-codebox-workspace/packages/cli/dist/index.js"
mkdir -p "$(dirname "${CLI}")"
cat > "${CLI}" <<'NODE'
#!/usr/bin/env node
if (process.argv.includes('commands')) process.exit(0);
if (process.argv.includes('--version')) { process.stdout.write('0.23.3\n'); process.exit(0); }
if (process.argv.includes('doctor')) { process.stdout.write(process.env.DOCTOR_OUTPUT || ''); process.exit(Number(process.env.DOCTOR_STATUS || 0)); }
NODE
chmod +x "${CLI}"

INSPECTOR="${ROOT_DIR}/scripts/build/inspect-wp-codebox-freshness.mjs"

doctor_json() {
    local source_status="$1"
    local version="${2:-0.23.3}"
    local dist="${3:-fresh-dist}"
    local ref="${4:-main}"
    local commit="${5:-fresh-commit}"
    local evidence="${6:-local-tracking-ref}"
    jq -cn --arg status "${source_status}" --arg version "${version}" --arg dist "${dist}" --arg ref "${ref}" --arg commit "${commit}" --arg evidence "${evidence}" '{schema:"wp-codebox/doctor/v1",status:(if $status == "error" then "error" elif $status == "warning" then "warning" else "ok" end),checks:[{id:"wp-codebox.source",status:$status,message:"fixture provenance",details:{provenance:{schema:"wp-codebox/cli-build-provenance/v1",package:{name:"@automattic/wp-codebox-cli",version:$version},dist:{sha256:$dist},git:{ref:$ref,commit:$commit}},git:{evidence:$evidence,remoteFetch:"not-attempted"}}}]}'
}

expect_accept() {
    local name="$1"
    shift
    if ! DOCTOR_OUTPUT="${DOCTOR_OUTPUT}" node "${INSPECTOR}" --bin "${CLI}" "$@" > "${TMPDIR}/${name}.json"; then
        echo "Expected ${name} to be accepted" >&2
        cat "${TMPDIR}/${name}.json" >&2
        exit 1
    fi
    jq -e '.fresh == true' "${TMPDIR}/${name}.json" >/dev/null
}

expect_reject() {
    local name="$1"
    local reason="$2"
    shift 2
    if DOCTOR_OUTPUT="${DOCTOR_OUTPUT}" node "${INSPECTOR}" --bin "${CLI}" "$@" > "${TMPDIR}/${name}.json"; then
        echo "Expected ${name} to be rejected" >&2
        exit 1
    fi
    jq -e --arg reason "${reason}" '.fresh == false and .reason == $reason' "${TMPDIR}/${name}.json" >/dev/null
}

# A runnable global workspace package cannot satisfy a newer managed release.
DOCTOR_OUTPUT="$(doctor_json ok 0.21.0 stale-dist)"
expect_reject stale-global release_identity_mismatch --candidate ambient --mode release --expected-version 0.23.3 --expected-dist fresh-dist

DOCTOR_OUTPUT="$(doctor_json ok)"
expect_accept fresh-release --candidate managed --mode release --expected-version 0.23.3 --expected-dist fresh-dist
expect_accept rebuilt-source --candidate managed --mode source --expected-ref main --expected-commit fresh-commit

DOCTOR_OUTPUT='not-json'
expect_reject malformed-doctor doctor_json_malformed --candidate managed --mode release --expected-version 0.23.3 --expected-dist fresh-dist

DOCTOR_OUTPUT='{"schema":"wp-codebox/doctor/v1","status":"warning","checks":[]}'
expect_reject missing-provenance provenance_check_missing --candidate managed --mode release --expected-version 0.23.3 --expected-dist fresh-dist

DOCTOR_OUTPUT="$(doctor_json warning 0.23.3 fresh-dist main fresh-commit unavailable)"
expect_reject unavailable-provenance provenance_unavailable --candidate managed --mode source --expected-ref main

DOCTOR_OUTPUT="$(doctor_json ok)"
expect_reject override-without-commit source_authority_unprovable --candidate override --mode source --expected-ref main
expect_accept override-exact --candidate override --mode source --expected-ref main --expected-commit fresh-commit

# Managed source may remain usable offline when immutable source/dist evidence is
# current; doctor explicitly reports that no fetch was attempted.
DOCTOR_OUTPUT="$(doctor_json ok 0.23.3 fresh-dist main fresh-commit unavailable)"
expect_accept offline-current-managed --candidate managed --mode source --expected-ref main

echo "WP Codebox freshness inspector smoke passed"
