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
  if [[ -z "${token}" ]]; then
    return 0
  fi

  local askpass
  askpass="$(mktemp)"
  chmod 700 "${askpass}"
  cat > "${askpass}" <<'SH'
#!/usr/bin/env bash
case "$1" in
  *Username*) printf '%s\n' 'x-access-token' ;;
  *Password*) printf '%s\n' "${HOMEBREW_TAP_TOKEN:-${GH_TOKEN:-}}" ;;
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
    if [[ -z "${HOMEBREW_TAP_TOKEN:-${GH_TOKEN:-}}" ]]; then
      echo "Homebrew tap publishing requires HOMEBREW_TAP_TOKEN or GH_TOKEN." >&2
      exit 1
    fi

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

# Get current package info from Cargo.toml
PACKAGE_NAME=$(cargo metadata --format-version 1 --no-deps 2>/dev/null | jq -r '.packages[0].name // empty')
CURRENT_VERSION=$(cargo metadata --format-version 1 --no-deps 2>/dev/null | jq -r '.packages[0].version // empty')

if [[ -z "$PACKAGE_NAME" || -z "$CURRENT_VERSION" ]]; then
  echo "Failed to read package info from Cargo.toml" >&2
  exit 1
fi

# Check if this version is already published on crates.io
PUBLISHED_VERSION=$(cargo search "$PACKAGE_NAME" --limit 1 2>/dev/null | grep -oE "^$PACKAGE_NAME = \"[^\"]+\"" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || echo "")

if [[ "$CURRENT_VERSION" == "$PUBLISHED_VERSION" ]]; then
  echo "Version $CURRENT_VERSION of $PACKAGE_NAME already published to crates.io, skipping..."
  publish_homebrew_formulae
  exit 0
fi

echo "Publishing $PACKAGE_NAME v$CURRENT_VERSION to crates.io..."
cargo publish --locked --allow-dirty

publish_homebrew_formulae
