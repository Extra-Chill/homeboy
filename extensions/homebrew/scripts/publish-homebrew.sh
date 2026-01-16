#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${HOMEBOY_SETTINGS_JSON:-}" ]]; then
  echo "Missing HOMEBOY_SETTINGS_JSON" >&2
  exit 1
fi

payload=$(echo "$HOMEBOY_SETTINGS_JSON" | jq -r '.release // empty')
if [[ -z "$payload" ]]; then
  echo "Missing release payload" >&2
  exit 1
fi

artifacts=()
while IFS= read -r path; do
  [[ -n "$path" ]] && artifacts+=("$path")
done <<< "$(echo "$payload" | jq -r '.artifacts // [] | .[] | select(.path | endswith(".rb")) | .path')"

if [[ ${#artifacts[@]} -eq 0 ]]; then
  echo "No .rb artifacts provided" >&2
  exit 1
fi

tap_repo=$(echo "$HOMEBOY_SETTINGS_JSON" | jq -r '.config.tap_repo // empty')
tap_repo=${tap_repo:-"Extra-Chill/homebrew-tap"}

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

gh repo clone "${tap_repo}" "$tmp_dir"

for formula in "${artifacts[@]}"; do
  if [[ ! -f "$formula" ]]; then
    echo "Formula not found: $formula" >&2
    exit 1
  fi
  cp "$formula" "$tmp_dir/Formula/"
done

cd "$tmp_dir"

git config user.name "${GITHUB_USER:-Homeboy}"
git config user.email "${GITHUB_EMAIL:-homeboy@localhost}"

git add Formula/*.rb
if git diff --cached --quiet; then
  echo "No formula changes to publish"
  exit 0
fi

git commit -m "Update Homebrew formulae"
git push origin main
