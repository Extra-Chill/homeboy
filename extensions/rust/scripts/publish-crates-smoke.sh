#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "$0")/../.." && pwd)
SCRIPT="${ROOT_DIR}/rust/scripts/publish-crates.sh"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "${TMP_DIR}"' EXIT

BIN_DIR="${TMP_DIR}/bin"
mkdir -p "${BIN_DIR}"

cat >"${BIN_DIR}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

case "$1" in
  metadata)
    printf '{"workspace_members":["homeboy-smoke 1.2.3"],"packages":[{"id":"homeboy-smoke 1.2.3","name":"homeboy-smoke","version":"1.2.3","publish":null}],"resolve":{"nodes":[{"id":"homeboy-smoke 1.2.3","deps":[]}]}}'
    ;;
  info)
    [[ -f "${CARGO_PUBLISHED_MARKER}" ]]
    ;;
  publish)
    printf '%s\n' "$@" >"${CARGO_PUBLISH_ARGS_LOG}"
    touch "${CARGO_PUBLISHED_MARKER}"
    ;;
  *)
    echo "unexpected cargo command: $*" >&2
    exit 1
    ;;
esac
SH
chmod +x "${BIN_DIR}/cargo"

export PATH="${BIN_DIR}:${PATH}"
export CARGO_PUBLISH_ARGS_LOG="${TMP_DIR}/publish-args.log"
export CARGO_PUBLISHED_MARKER="${TMP_DIR}/published"

bash "${SCRIPT}" >/dev/null

if ! grep -qx -- '--allow-dirty' "${CARGO_PUBLISH_ARGS_LOG}"; then
  echo "FAIL: cargo publish did not receive --allow-dirty" >&2
  cat "${CARGO_PUBLISH_ARGS_LOG}" >&2
  exit 1
fi

if ! grep -qx -- '--package' "${CARGO_PUBLISH_ARGS_LOG}" || ! grep -qx -- 'homeboy-smoke' "${CARGO_PUBLISH_ARGS_LOG}"; then
  echo "FAIL: cargo publish did not receive the explicit package identity" >&2
  cat "${CARGO_PUBLISH_ARGS_LOG}" >&2
  exit 1
fi

if ! grep -qx -- '--locked' "${CARGO_PUBLISH_ARGS_LOG}"; then
  echo "FAIL: cargo publish did not preserve --locked" >&2
  cat "${CARGO_PUBLISH_ARGS_LOG}" >&2
  exit 1
fi

echo "OK: publish-crates.sh passes --allow-dirty to cargo publish"
