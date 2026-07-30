#!/usr/bin/env bash
set -euo pipefail

release_payload() {
  if [[ -n "${HOMEBOY_SETTINGS_JSON:-}" ]]; then
    printf '%s' "${HOMEBOY_SETTINGS_JSON}"
  else
    printf '{}'
  fi
}

release_version() {
  release_payload | jq -r '.release.version // empty'
}

dist_homebrew_tap() {
  if [[ ! -f dist-workspace.toml ]]; then
    return 0
  fi

  awk -F'"' '/^[[:space:]]*tap[[:space:]]*=/ { print $2; exit }' dist-workspace.toml
}

homebrew_formula_artifacts() {
  release_payload | jq -r '
    .release.artifacts // []
    | .[]
    | select((.type // .artifact_type // "") == "homebrew" or ((.path // "") | endswith(".rb")))
    | .path
  '
}

configure_git_push_auth() {
  local token="${HOMEBREW_TAP_TOKEN:-${GH_TOKEN:-}}"
  if [[ -z "${token}" ]] && command -v gh &>/dev/null; then
    token="$(gh auth token 2>/dev/null || true)"
  fi

  if [[ -z "${token}" ]]; then
    echo "Homebrew tap publishing requires HOMEBREW_TAP_TOKEN, GH_TOKEN, or an authenticated gh CLI." >&2
    return 1
  fi

  export HOMEBREW_TAP_AUTH_TOKEN="${token}"

  local askpass
  askpass="$(mktemp)"
  chmod 700 "${askpass}"
  cat > "${askpass}" <<'SH'
#!/usr/bin/env bash
case "$1" in
  *Username*) printf '%s\n' 'x-access-token' ;;
  *Password*) printf '%s\n' "${HOMEBREW_TAP_AUTH_TOKEN}" ;;
  *) printf '\n' ;;
esac
SH

  export GIT_ASKPASS="${askpass}"
  export GIT_TERMINAL_PROMPT=0
  trap 'rm -f "${GIT_ASKPASS:-}"' EXIT
}

publish_homebrew_formulae() {
  if ! command -v jq &>/dev/null; then
    echo "Error: jq is required for Homebrew formula publishing." >&2
    exit 1
  fi

  local tap formulae
  tap="${HOMEBREW_TAP_REPOSITORY:-$(dist_homebrew_tap)}"
  if [[ -z "${tap}" ]]; then
    echo "No Homebrew tap configured, skipping formula publish."
    return 0
  fi

  formulae="$(homebrew_formula_artifacts)"
  if [[ -z "${formulae}" ]]; then
    echo "No Homebrew formula artifacts found, skipping formula publish."
    return 0
  fi

  local tap_dir cleanup_tap
  cleanup_tap="false"
  if [[ -n "${HOMEBOY_HOMEBREW_TAP_DIR:-}" ]]; then
    tap_dir="${HOMEBOY_HOMEBREW_TAP_DIR}"
  else
    configure_git_push_auth
    tap_dir="$(mktemp -d)"
    cleanup_tap="true"
    git clone "https://github.com/${tap}.git" "${tap_dir}"
  fi

  mkdir -p "${tap_dir}/Formula"

  local formula copied=0
  while IFS= read -r formula; do
    [[ -z "${formula}" ]] && continue
    if [[ ! -f "${formula}" ]]; then
      echo "Expected Homebrew formula artifact not found: ${formula}" >&2
      exit 1
    fi

    cp "${formula}" "${tap_dir}/Formula/$(basename "${formula}")"
    copied=$((copied + 1))
  done <<< "${formulae}"

  if [[ "${copied}" -eq 0 ]]; then
    echo "No Homebrew formula artifacts copied, skipping formula publish."
    return 0
  fi

  if [[ "${HOMEBOY_HOMEBREW_SKIP_STYLE:-false}" != "true" ]] && command -v brew &>/dev/null; then
    while IFS= read -r formula; do
      [[ -z "${formula}" ]] && continue
      brew style --except-cops FormulaAudit/Homepage,FormulaAudit/Desc,FormulaAuditStrict --fix "${tap_dir}/Formula/$(basename "${formula}")" || true
    done <<< "${formulae}"
  fi

  git -C "${tap_dir}" add Formula
  if git -C "${tap_dir}" diff --cached --quiet; then
    echo "Homebrew tap is already up to date."
    [[ "${cleanup_tap}" == "true" ]] && rm -rf "${tap_dir}"
    return 0
  fi

  git -C "${tap_dir}" config user.name "${HOMEBREW_GIT_USER:-Extra Chill Bot}"
  git -C "${tap_dir}" config user.email "${HOMEBREW_GIT_EMAIL:-bot@extrachill.com}"

  local version first_formula name
  version="$(release_version)"
  first_formula="$(printf '%s\n' "${formulae}" | awk 'NF { print; exit }')"
  name="$(basename "${first_formula}" .rb)"
  git -C "${tap_dir}" commit -m "${name} ${version}"

  if [[ "${HOMEBOY_HOMEBREW_SKIP_PUSH:-false}" == "true" ]]; then
    echo "Homebrew tap push skipped by HOMEBOY_HOMEBREW_SKIP_PUSH."
  else
    git -C "${tap_dir}" push
  fi

  if [[ "${cleanup_tap}" == "true" ]]; then
    rm -rf "${tap_dir}"
  fi

  return 0
}

workspace_packages() {
  cargo metadata --format-version 1 --locked | jq -r '
    (.workspace_members // [.packages[].id]) as $members
    | [.packages[]
       | select(.id as $id | $members | index($id))
       | select(.publish != [])
       | { id, name, version }
      ] as $packages
    | ($packages | map(.id)) as $publishable_ids
    | [ $packages[] as $package
        | {
            id: $package.id,
            name: $package.name,
            version: $package.version,
            dependencies: [
              (.resolve.nodes[]? | select(.id == $package.id) | .deps[]?.pkg)
              | select(. as $dependency | $publishable_ids | index($dependency))
            ]
          }
      ] as $pending
    | { pending: $pending, published: [] }
    | until(
        (.pending | length) == 0;
        . as $state
        | ([.pending[]
            | select((.dependencies - $state.published | length) == 0)
          ] | sort_by(.name)) as $ready
        | if ($ready | length) == 0 then
            error("publishable workspace packages contain a dependency cycle")
          else
            .published += ($ready | map(.id))
            | .pending -= $ready
          end
      )
    | .published[] as $id
    | $pending[] | select(.id == $id) | [.name, .version] | @tsv
  '
}

package_is_published() {
  cargo info "$1@$2" --registry crates-io >/dev/null 2>&1
}

wait_for_registry_visibility() {
  local package_name="$1" package_version="$2"
  local attempts="${HOMEBOY_CRATES_IO_VISIBILITY_ATTEMPTS:-12}"
  local delay="${HOMEBOY_CRATES_IO_VISIBILITY_DELAY_SECONDS:-5}"
  local attempt

  for ((attempt = 1; attempt <= attempts; attempt++)); do
    if package_is_published "${package_name}" "${package_version}"; then
      echo "${package_name} v${package_version} is available in the crates.io registry."
      return 0
    fi

    if [[ "${attempt}" -lt "${attempts}" ]]; then
      echo "Waiting for ${package_name} v${package_version} to become available in crates.io (${attempt}/${attempts})..."
      sleep "${delay}"
    fi
  done

  echo "${package_name} v${package_version} did not become available in crates.io after ${attempts} checks." >&2
  return 1
}

is_crates_io_rate_limited() {
  local output
  for output in "$@"; do
    if grep -qi 'crates\.io' "${output}" && grep -Eqi '(^|[^0-9])429([^0-9]|$)|rate limit' "${output}"; then
      return 0
    fi
  done

  return 1
}

crates_io_retry_delay() {
  local output retry_after retry_after_epoch now delay
  local fallback_delay="${HOMEBOY_CRATES_IO_PUBLISH_RETRY_DELAY_SECONDS:-60}"
  local safety_margin="${HOMEBOY_CRATES_IO_PUBLISH_RETRY_SAFETY_MARGIN_SECONDS:-5}"
  local max_delay="${HOMEBOY_CRATES_IO_PUBLISH_RETRY_MAX_DELAY_SECONDS:-300}"

  if ! [[ "${fallback_delay}" =~ ^[0-9]+$ && "${safety_margin}" =~ ^[0-9]+$ && "${max_delay}" =~ ^[1-9][0-9]*$ ]]; then
    echo "Invalid crates.io publish retry delay configuration." >&2
    return 1
  fi

  retry_after=""
  while IFS= read -r output; do
    if [[ "${output}" =~ Please[[:space:]]try[[:space:]]again[[:space:]]after[[:space:]]+([A-Z][a-z][a-z],[[:space:]][0-9]{1,2}[[:space:]][A-Z][a-z][a-z][[:space:]][0-9]{4}[[:space:]][0-9]{2}:[0-9]{2}:[0-9]{2}[[:space:]]GMT) ]]; then
      retry_after="${BASH_REMATCH[1]}"
      break
    fi
  done < <(cat "$@")

  delay="${fallback_delay}"
  if [[ -n "${retry_after}" ]] && { retry_after_epoch="$(date -u -j -f '%a, %d %b %Y %H:%M:%S %Z' "${retry_after}" +%s 2>/dev/null)" || retry_after_epoch="$(date -u -d "${retry_after}" +%s 2>/dev/null)"; }; then
    now="$(date +%s)"
    if (( retry_after_epoch > now )); then
      delay=$((retry_after_epoch - now + safety_margin))
    fi
  fi

  if (( delay > max_delay )); then
    delay="${max_delay}"
  fi

  printf '%s\n' "${delay}"
}

publish_package() {
  local package_name="$1"
  local attempts="${HOMEBOY_CRATES_IO_PUBLISH_ATTEMPTS:-3}"
  local attempt status delay stdout stderr

  if ! [[ "${attempts}" =~ ^[1-9][0-9]*$ ]]; then
    echo "Invalid HOMEBOY_CRATES_IO_PUBLISH_ATTEMPTS value: ${attempts}" >&2
    return 1
  fi

  for ((attempt = 1; attempt <= attempts; attempt++)); do
    stdout="$(mktemp)"
    stderr="$(mktemp)"

    if cargo publish --package "${package_name}" --locked --allow-dirty >"${stdout}" 2>"${stderr}"; then
      cat "${stdout}"
      cat "${stderr}" >&2
      rm -f "${stdout}" "${stderr}"
      return 0
    else
      status=$?
    fi

    cat "${stdout}"
    cat "${stderr}" >&2

    if ! is_crates_io_rate_limited "${stdout}" "${stderr}" || [[ "${attempt}" -eq "${attempts}" ]]; then
      rm -f "${stdout}" "${stderr}"
      return "${status}"
    fi

    delay="$(crates_io_retry_delay "${stdout}" "${stderr}")" || {
      rm -f "${stdout}" "${stderr}"
      return 1
    }
    rm -f "${stdout}" "${stderr}"
    echo "crates.io rate limited ${package_name}; retrying in ${delay}s (${attempt}/${attempts})..." >&2
    sleep "${delay}"
  done
}

if ! command -v jq &>/dev/null; then
  echo "Error: jq is required for Rust workspace publishing." >&2
  exit 1
fi

while IFS=$'\t' read -r package_name package_version; do
  [[ -z "${package_name}" || -z "${package_version}" ]] && continue

  if package_is_published "${package_name}" "${package_version}"; then
    echo "${package_name} v${package_version} is already published to crates.io."
    continue
  fi

  echo "Publishing ${package_name} v${package_version} to crates.io..."
  publish_package "${package_name}"
  wait_for_registry_visibility "${package_name}" "${package_version}"
done < <(workspace_packages)

publish_homebrew_formulae
