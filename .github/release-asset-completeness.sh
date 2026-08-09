#!/usr/bin/env bash
#
# Deterministic asset-completeness contract for a Homeboy GitHub Release.
#
# WHY THIS EXISTS (#11749, and #8687 before it)
#
# `verify-published` already re-drafts an incomplete release — but it is a job
# inside `release.yml`, so it can only fire when `release.yml` actually runs the
# publish chain. v0.333.0 was published at 17:15:59Z on 2026-08-06 with 7 of 13
# assets and no Linux binary at all, and NO release run executed a single job in
# that window: 27 of the last 30 runs had `total_count: 0` jobs, having been
# displaced while still pending in the `release-refs/heads/main` concurrency
# group. A guard that lives only on the happy path of a pipeline that is not
# running is not a guard.
#
# So the contract also lives here: one script, no cargo-dist invocation, no
# network beyond a single inventory read, callable from `release-integrity.yml`
# on `release: published` and on a schedule. That catches every publication,
# including a local `homeboy release` from a laptop, which is what a 7-asset
# macOS-only inventory looks like.
#
# `release.yml` cannot call this before finalization: absent-release recovery
# intentionally has a tag but no remote Release object for `gh release view` to
# inspect. The owning `github.release` finalizer creates that object as a draft,
# validates its declared artifacts, and only then publishes it.
#
# The release workflow passes cargo-dist's planned asset inventory directly.
# That plan is the authoritative contract: it includes the configured platform
# archives, their checksums, and installer assets without reconstructing names
# from a particular product. The config-derived fallback keeps the independent
# integrity sweeper able to audit releases outside the workflow.
#
# Environment:
#   RELEASE_TAG              (required) tag whose inventory is checked
#   GITHUB_REPOSITORY        (optional) owner/repo, for `gh`
#   DIST_WORKSPACE           (optional) path to dist-workspace.toml
#   RELEASE_INVENTORY_JSON   (optional) pre-fetched `{"assets":[...]}` JSON.
#                            When set, no `gh` call is made. This is the seam
#                            the Rust tests drive the contract through.
#   EXPECTED_ASSETS          (optional) JSON array emitted by cargo-dist's
#                            release plan. When set, this is the required
#                            inventory contract.
#   REQUIRE_ANNOUNCE_ASSETS  (optional, default true) require assets cargo-dist
#                            attaches during announce (`dist-manifest.json`).
#                            Set false for the pre-publication gate, where the
#                            upload step has run but announce has not.
#   ASSET_DIR                (optional) rebuilt local assets. When set, every
#                            required file and checksum contract is verified
#                            against GitHub's authoritative asset digests.
#   RECONCILE                (optional, default false) replace missing, empty,
#                            or digest-mismatched expected assets from ASSET_DIR.
#
# Exit 0 = every required asset is present, uploaded and non-empty.
# Exit 1 = incomplete, unreadable, or underivable. This gate fails CLOSED:
#          failing a release run is strictly better than publishing an
#          inventory that 404s for the platforms it dropped.

set -uo pipefail

DIST_WORKSPACE="${DIST_WORKSPACE:-dist-workspace.toml}"
REQUIRE_ANNOUNCE_ASSETS="${REQUIRE_ANNOUNCE_ASSETS:-true}"
RELEASE_TAG="${RELEASE_TAG:-}"
ASSET_DIR="${ASSET_DIR:-}"
RECONCILE="${RECONCILE:-false}"

# Deliberately no `$GITHUB_OUTPUT` writes. This script's verdict IS its exit
# status, and both callers consume it that way. Emitting a `complete=true`
# boolean as well would invent a second, redundant gating output that a
# downstream `if:` could read -- exactly the class the gate-layer measurement
# registry exists to keep accounted for.
fail() {
  echo "::error::$1"
  exit 1
}

if [ -z "${RELEASE_TAG}" ]; then
  fail "RELEASE_TAG is required to check release asset completeness"
fi

REQUIRED=()
CONTRACT_SOURCE="cargo-dist release plan"
if [ -n "${EXPECTED_ASSETS:-}" ]; then
  if ! jq -e 'type == "array" and length > 0 and all(.[]; type == "string" and length > 0)' >/dev/null 2>&1 <<< "${EXPECTED_ASSETS}"; then
    fail "The cargo-dist expected asset contract is not a non-empty JSON array of names. Refusing to treat an underivable contract as satisfied."
  fi
  while IFS= read -r asset; do
    REQUIRED+=("${asset}")
  done < <(jq -r '.[]' <<< "${EXPECTED_ASSETS}")
else
  # The sweeper has no plan-job output, so derive the portable cargo-dist
  # fallback from the declared target matrix.
  CONTRACT_SOURCE="${DIST_WORKSPACE}"
  if [ ! -f "${DIST_WORKSPACE}" ]; then
    fail "Cannot derive the release asset contract: ${DIST_WORKSPACE} is missing. Refusing to treat an underivable contract as a satisfied one."
  fi

  TARGETS_LINE="$(grep -E '^[[:space:]]*targets[[:space:]]*=' "${DIST_WORKSPACE}" | head -n 1)"
  TARGETS=()
  if [ -n "${TARGETS_LINE}" ]; then
    while IFS= read -r target; do
      [ -n "${target}" ] && TARGETS+=("${target}")
    done < <(printf '%s\n' "${TARGETS_LINE}" | grep -oE '"[^"]+"' | tr -d '"')
  fi

  if [ "${#TARGETS[@]}" -eq 0 ]; then
    fail "Cannot derive the release asset contract: no \`targets\` declared in ${DIST_WORKSPACE}. An empty platform contract would silently pass every incomplete release."
  fi

  PACKAGE_NAME="$(awk '
    /^\[package\]$/ { in_package = 1; next }
    /^\[/ { in_package = 0 }
    in_package && /^[[:space:]]*name[[:space:]]*=/ {
      sub(/^[^"]*"/, "")
      sub(/".*$/, "")
      print
      exit
    }
  ' Cargo.toml)"
  if [ -z "${PACKAGE_NAME}" ]; then
    fail "Cannot derive the release asset contract: Cargo.toml has no package name."
  fi

  for target in "${TARGETS[@]}"; do
    REQUIRED+=("${PACKAGE_NAME}-${target}.tar.xz" "${PACKAGE_NAME}-${target}.tar.xz.sha256")
  done
  REQUIRED+=("source.tar.gz" "source.tar.gz.sha256" "sha256.sum")

  if grep -qE '^[[:space:]]*installers[[:space:]]*=.*homebrew' "${DIST_WORKSPACE}"; then
    REQUIRED+=("${PACKAGE_NAME}.rb")
  fi

  if [ "${REQUIRE_ANNOUNCE_ASSETS}" = "true" ]; then
    REQUIRED+=("dist-manifest.json")
  fi
fi

# cargo-dist attaches this during announce, after the pre-publication upload
# phase. It remains required at the final published/success boundary.
if [ "${REQUIRE_ANNOUNCE_ASSETS}" != "true" ]; then
  PRE_ANNOUNCE_REQUIRED=()
  for asset in "${REQUIRED[@]}"; do
    [ "${asset}" != "dist-manifest.json" ] && PRE_ANNOUNCE_REQUIRED+=("${asset}")
  done
  REQUIRED=("${PRE_ANNOUNCE_REQUIRED[@]}")
fi

# ── Observed inventory ──
INVENTORY=""
# Presence, not non-emptiness. An explicitly supplied but EMPTY inventory means
# "I looked and found nothing readable", which must fail closed below — falling
# back to a live `gh` call there would turn an unreadable observation into a
# fresh one and hide the very condition being tested.
if [ -n "${RELEASE_INVENTORY_JSON+set}" ]; then
  INVENTORY="${RELEASE_INVENTORY_JSON}"
else
  GH_ARGS=(release view "${RELEASE_TAG}" --json isDraft,assets)
  if [ -n "${GITHUB_REPOSITORY:-}" ]; then
    GH_ARGS+=(--repo "${GITHUB_REPOSITORY}")
  fi
  if ! INVENTORY="$(gh "${GH_ARGS[@]}" 2>&1)"; then
    fail "Could not read the asset inventory for ${RELEASE_TAG}: ${INVENTORY}"
  fi
fi

if ! jq -e '.assets' >/dev/null 2>&1 <<< "${INVENTORY}"; then
  fail "The asset inventory for ${RELEASE_TAG} is not readable JSON with an .assets array. An unreadable inventory is UNKNOWN, not complete."
fi

USABLE="$(jq -r '[.assets[]? | select(.state == "uploaded" and .size > 0) | .name] | .[]' <<< "${INVENTORY}")"

MISSING=()
for asset in "${REQUIRED[@]}"; do
  if ! printf '%s\n' "${USABLE}" | grep -Fqx "${asset}"; then
    MISSING+=("${asset}")
  fi
done

if [ "${#MISSING[@]}" -gt 0 ] && [ "${RECONCILE}" != "true" ]; then
  echo "::error::Release ${RELEASE_TAG} does not satisfy the declared asset contract. Missing or unusable: ${MISSING[*]}"
  echo "::error::Derived from ${CONTRACT_SOURCE}: ${#REQUIRED[@]} assets required. A release that cannot satisfy its own platform matrix must not be published — consumers on the missing platforms get a 404 from \`homeboy upgrade\` (#11749)."
  exit 1
fi

# A set comparison without cardinality would hide duplicate names. Rejecting
# extras is intentional: a recovery must never publish an inventory whose
# bytes were not rebuilt and checked by this run.
if ! jq -e '
  .assets | type == "array" and
  all(.[]; type == "object" and (.name | type == "string" and length > 0))
' >/dev/null 2>&1 <<< "${INVENTORY}"; then
  fail "The asset inventory for ${RELEASE_TAG} has malformed asset records. An unreadable inventory is UNKNOWN, not complete."
fi

EXPECTED_JSON="$(printf '%s\n' "${REQUIRED[@]}" | jq -Rsc 'split("\n") | map(select(length > 0)) | sort')"
ACTUAL_JSON="$(jq -Sc '[.assets[].name] | sort' <<< "${INVENTORY}")"
EXPECTED_COUNT="$(jq -r 'length' <<< "${EXPECTED_JSON}")"
ACTUAL_COUNT="$(jq -r 'length' <<< "${ACTUAL_JSON}")"
if [ "${RECONCILE}" = "true" ]; then
  if ! jq -e --argjson expected "${EXPECTED_JSON}" '
    [.assets[].name] as $names |
    ($names | all(.[]; . as $name | $expected | index($name))) and
    ($names | length) == ($names | unique | length)
  ' >/dev/null <<< "${INVENTORY}"; then
    fail "Release ${RELEASE_TAG} inventory has unexpected or duplicate assets. Recovery only replaces expected assets and refuses to publish an unowned inventory."
  fi
elif [ "${EXPECTED_JSON}" != "${ACTUAL_JSON}" ] || [ "${EXPECTED_COUNT}" != "${ACTUAL_COUNT}" ]; then
  fail "Release ${RELEASE_TAG} inventory is not the exact expected asset set (expected ${EXPECTED_COUNT}: ${EXPECTED_JSON}; observed ${ACTUAL_COUNT}: ${ACTUAL_JSON}). Unexpected or duplicate assets must be removed before publication."
fi

if [ -z "${ASSET_DIR}" ]; then
  echo "::notice::Release ${RELEASE_TAG} satisfies the declared asset contract (${#REQUIRED[@]} assets from ${CONTRACT_SOURCE})."
  exit 0
fi

[ -d "${ASSET_DIR}" ] || fail "Rebuilt asset directory ${ASSET_DIR} does not exist"

# Verify local bytes first. The checksum sidecars are the rebuilt contract for
# payloads; GitHub's digest must then agree with the actual rebuilt bytes.
declare -A LOCAL_DIGESTS=()
declare -A CONTRACT_DIGESTS=()
for asset in "${REQUIRED[@]}"; do
  path="${ASSET_DIR}/${asset}"
  [ -f "${path}" ] && [ -s "${path}" ] || fail "Rebuilt recovery asset ${path} is missing or empty"
  LOCAL_DIGESTS["${asset}"]="sha256:$(sha256sum "${path}" | cut -d' ' -f1)"
done

payload_count=0
for asset in "${REQUIRED[@]}"; do
  case "${asset}" in *.sha256|sha256.sum) continue ;; esac
  payload_count=$((payload_count + 1))
done
[ "${payload_count}" -gt 0 ] || fail "The rebuilt asset contract has no payloads"

for sidecar in "${REQUIRED[@]}"; do
  case "${sidecar}" in *.sha256|sha256.sum) ;; *) continue ;; esac
  references=0
  while read -r digest name extra; do
    [ -n "${digest}" ] || continue
    name="${name#\*}"
    [[ "${digest}" =~ ^[[:xdigit:]]{64}$ ]] || fail "Invalid checksum contract in ${sidecar}"
    [ -n "${name}" ] && [ -z "${extra:-}" ] || fail "Invalid checksum contract in ${sidecar}"
    case "${name}" in *.sha256|sha256.sum) fail "Invalid checksum contract in ${sidecar}" ;; esac
    [ -n "${LOCAL_DIGESTS[${name}]:-}" ] || fail "Checksum contract ${sidecar} references unexpected payload ${name}"
    expected="sha256:${digest,,}"
    [ "${LOCAL_DIGESTS[${name}]}" = "${expected}" ] || fail "Checksum contract ${sidecar} does not match rebuilt payload ${name}"
    [ -z "${CONTRACT_DIGESTS[${name}]:-}" ] || [ "${CONTRACT_DIGESTS[${name}]}" = "${expected}" ] || fail "Checksum contracts disagree for ${name}"
    CONTRACT_DIGESTS["${name}"]="${expected}"
    references=$((references + 1))
  done < "${ASSET_DIR}/${sidecar}"
  [ "${references}" -gt 0 ] || fail "Checksum contract ${sidecar} is empty"
  case "${sidecar}" in *.sha256) [ "${references}" -eq 1 ] && [ "${sidecar%.sha256}" = "${name}" ] || fail "Checksum contract ${sidecar} is incomplete" ;; esac
done
for asset in "${REQUIRED[@]}"; do
  case "${asset}" in *.sha256|sha256.sum) continue ;; esac
  [ -n "${CONTRACT_DIGESTS[${asset}]:-}" ] || fail "No rebuilt checksum contract covers ${asset}"
done

remote_digest() {
  jq -r --arg name "$1" '.assets[] | select(.name == $name) | .digest // empty' <<< "${INVENTORY}"
}
remote_valid() {
  local name="$1"
  jq -e --arg name "${name}" --arg digest "${LOCAL_DIGESTS[${name}]}" \
    '.assets[] | select(.name == $name and .state == "uploaded" and .size > 0 and .digest == $digest)' \
    >/dev/null <<< "${INVENTORY}"
}

if [ "${RECONCILE}" = "true" ]; then
  for asset in "${REQUIRED[@]}"; do
    if remote_valid "${asset}"; then
      echo "::notice::Retaining verified rebuilt release asset ${asset}"
    else
      gh release upload "${RELEASE_TAG}" "${ASSET_DIR}/${asset}" --clobber
      echo "::notice::Replaced recovery release asset ${asset}"
    fi
  done
  if ! INVENTORY="$(gh release view "${RELEASE_TAG}" --json assets 2>&1)"; then
    fail "Could not re-read the asset inventory for ${RELEASE_TAG}: ${INVENTORY}"
  fi
  FINAL_JSON="$(jq -Sc '[.assets[].name] | sort' <<< "${INVENTORY}" 2>/dev/null)" || fail "The final asset inventory for ${RELEASE_TAG} is unreadable"
  FINAL_COUNT="$(jq -r 'length' <<< "${FINAL_JSON}")"
  if [ "${EXPECTED_JSON}" != "${FINAL_JSON}" ] || [ "${EXPECTED_COUNT}" != "${FINAL_COUNT}" ]; then
    fail "Release ${RELEASE_TAG} final inventory is not the exact rebuilt asset set. No publication is permitted."
  fi
fi

# This is deliberately the final operation: no caller may publish after a
# stale read, a partial upload, or a concurrent inventory mutation.
for asset in "${REQUIRED[@]}"; do
  remote_valid "${asset}" || fail "Release ${RELEASE_TAG} asset ${asset} is not an uploaded non-empty GitHub asset with the rebuilt SHA-256 digest ${LOCAL_DIGESTS[${asset}]}"
done

echo "::notice::Release ${RELEASE_TAG} satisfies the declared asset contract (${#REQUIRED[@]} assets from ${CONTRACT_SOURCE})."
exit 0
