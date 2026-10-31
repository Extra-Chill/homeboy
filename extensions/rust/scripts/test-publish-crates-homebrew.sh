#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

PROJECT_DIR="${TMP_DIR}/project"
TAP_DIR="${TMP_DIR}/tap"
BIN_DIR="${TMP_DIR}/bin"
mkdir -p "${PROJECT_DIR}" "${TAP_DIR}/Formula" "${BIN_DIR}"

cat > "${PROJECT_DIR}/Cargo.toml" <<'TOML'
[package]
name = "homeboy"
version = "1.2.3"
edition = "2021"
TOML

cat > "${PROJECT_DIR}/dist-workspace.toml" <<'TOML'
[dist]
tap = "Extra-Chill/homebrew-tap"
TOML

cat > "${PROJECT_DIR}/homeboy.rb" <<'RUBY'
class Homeboy < Formula
  desc "Fixture"
  homepage "https://github.com/Extra-Chill/homeboy"
  version "1.2.3"
end
RUBY

cat > "${BIN_DIR}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  metadata)
    printf '{"workspace_members":["homeboy 1.2.3"],"packages":[{"id":"homeboy 1.2.3","name":"homeboy","version":"1.2.3","publish":null}],"resolve":{"nodes":[{"id":"homeboy 1.2.3","deps":[]}]}}'
    ;;
  info)
    printf 'cargo info %s\n' "$2" >> "${EVENT_LOG}"
    exit 0
    ;;
  publish)
    printf 'cargo publish should not run for an already-published version\n' >&2
    exit 1
    ;;
  *)
    printf 'unexpected cargo command: %s\n' "$*" >&2
    exit 1
    ;;
esac
SH
chmod +x "${BIN_DIR}/cargo"

REAL_GIT="$(command -v git)"
cat > "${BIN_DIR}/git" <<'SH'
#!/usr/bin/env bash
printf 'git %s\n' "$*" >> "${EVENT_LOG}"
exec "${REAL_GIT}" "$@"
SH
chmod +x "${BIN_DIR}/git"

git -C "${TAP_DIR}" init --initial-branch=main >/dev/null
git -C "${TAP_DIR}" config user.name "Fixture"
git -C "${TAP_DIR}" config user.email "fixture@example.com"
touch "${TAP_DIR}/.gitkeep"
git -C "${TAP_DIR}" add .gitkeep
git -C "${TAP_DIR}" commit -m initial >/dev/null

cd "${PROJECT_DIR}"
export EVENT_LOG="${TMP_DIR}/events.log"
export REAL_GIT
PATH="${BIN_DIR}:${PATH}" \
HOMEBOY_HOMEBREW_TAP_DIR="${TAP_DIR}" \
HOMEBOY_HOMEBREW_SKIP_PUSH=true \
HOMEBOY_HOMEBREW_SKIP_STYLE=true \
HOMEBOY_SETTINGS_JSON='{"release":{"version":"1.2.3","artifacts":[{"path":"homeboy.rb","type":"homebrew"}]}}' \
bash "${SCRIPT_DIR}/publish-crates.sh"

if ! cmp -s "${PROJECT_DIR}/homeboy.rb" "${TAP_DIR}/Formula/homeboy.rb"; then
  printf 'formula was not copied into tap\n' >&2
  exit 1
fi

LAST_SUBJECT="$(git -C "${TAP_DIR}" log -1 --format=%s)"
LAST_AUTHOR="$(git -C "${TAP_DIR}" log -1 --format='%an <%ae>')"

if [[ "${LAST_SUBJECT}" != "homeboy 1.2.3" ]]; then
  printf 'unexpected tap commit subject: %s\n' "${LAST_SUBJECT}" >&2
  exit 1
fi

if [[ "${LAST_AUTHOR}" != "Extra Chill Bot <bot@extrachill.com>" ]]; then
  printf 'unexpected tap commit author: %s\n' "${LAST_AUTHOR}" >&2
  exit 1
fi

FIRST_EVENT="$(awk 'NF { print; exit }' "${EVENT_LOG}")"
if [[ "${FIRST_EVENT}" != "cargo info homeboy@1.2.3" ]]; then
  printf 'Homebrew work began before Rust package publication completed: %s\n' "${FIRST_EVENT}" >&2
  exit 1
fi

printf 'publish-crates Homebrew tap test passed\n'
