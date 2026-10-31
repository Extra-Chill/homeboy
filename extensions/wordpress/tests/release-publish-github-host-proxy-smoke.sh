#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${ROOT_DIR}/scripts/release/publish.sh"
TMP_DIR="$(mktemp -d -t homeboy-release-publish-ghe.XXXXXX)"
trap 'rm -rf "${TMP_DIR}"' EXIT

BIN_DIR="${TMP_DIR}/bin"
WORK_DIR="${TMP_DIR}/work"
CAPTURE_DIR="${TMP_DIR}/capture"
mkdir -p "${BIN_DIR}" "${WORK_DIR}/build" "${CAPTURE_DIR}"

cat > "${BIN_DIR}/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'GH_HOST=%s\n' "${GH_HOST:-}"
  printf 'HTTPS_PROXY=%s\n' "${HTTPS_PROXY:-}"
  printf 'NO_PROXY=%s\n' "${NO_PROXY:-}"
  printf 'ARGS=%s\n' "$*"
} > "${HOMEBOY_GH_CAPTURE}"
SH
chmod +x "${BIN_DIR}/gh"

python3 -c "
import zipfile
with zipfile.ZipFile('${WORK_DIR}/build/demo-plugin.zip', 'w') as z:
    z.writestr('demo-plugin/demo-plugin.php', '<?php\n/**\n * Plugin Name: Demo Plugin\n * Version: 1.2.3\n */')
"

git -C "${WORK_DIR}" init -q
git -C "${WORK_DIR}" remote add origin git@github.enterprise.test:owner/demo-plugin.git

export PATH="${BIN_DIR}:${PATH}"
export HOMEBOY_GH_CAPTURE="${CAPTURE_DIR}/gh.env"
export HOMEBOY_SETTINGS_JSON='{
  "release": {
    "tag": "v1.2.3",
    "component_id": "demo-plugin"
  },
  "config": {
    "github": {
      "hosts": {
        "github.enterprise.test": {
          "proxy": "socks5://127.0.0.1:9999",
          "env": {
            "NO_PROXY": "localhost,127.0.0.1"
          }
        }
      }
    }
  }
}'

OUTPUT="$(cd "${WORK_DIR}" && bash "${SCRIPT}")"

grep -q '^GH_HOST=github.enterprise.test$' "${HOMEBOY_GH_CAPTURE}"
grep -q '^HTTPS_PROXY=socks5://127.0.0.1:9999$' "${HOMEBOY_GH_CAPTURE}"
grep -q '^NO_PROXY=localhost,127.0.0.1$' "${HOMEBOY_GH_CAPTURE}"
grep -q '^ARGS=release upload v1.2.3 build/demo-plugin.zip --clobber --repo owner/demo-plugin$' "${HOMEBOY_GH_CAPTURE}"

printf '%s' "${OUTPUT}" | jq -e '
  .success == true and
  .github_host == "github.enterprise.test" and
  .repository == "owner/demo-plugin"
' >/dev/null

printf 'release.publish GitHub host proxy smoke passed\n'
