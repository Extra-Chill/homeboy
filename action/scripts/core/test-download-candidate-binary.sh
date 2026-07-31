#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT
mkdir -p "${tmp}/bin"

cat > "${tmp}/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "$1" = api ]; then
  printf '%s\n' homeboy-candidate-binary-1 unrelated homeboy-candidate-binary-3 homeboy-candidate-binary-2
  exit 0
fi
if [ "$1 $2" = 'run download' ]; then
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --name) name="$2"; shift 2 ;;
      --dir) destination="$2"; shift 2 ;;
      *) shift ;;
    esac
  done
  printf '%s\n' "${name}" > "${destination}/homeboy"
  exit 0
fi
exit 1
SH
chmod +x "${tmp}/bin/gh"

PATH="${tmp}/bin:${PATH}" \
GITHUB_REPOSITORY=example/repo \
GITHUB_RUN_ID=42 \
GITHUB_OUTPUT="${tmp}/output" \
BINARY_DESTINATION="${tmp}/download" \
bash "${ROOT}/scripts/core/download-candidate-binary.sh" >/dev/null

grep -Fx 'artifact-name=homeboy-candidate-binary-3' "${tmp}/output" >/dev/null || { printf 'FAIL: newest binary attempt was not selected\n'; exit 1; }
expected="$(shasum -a 256 "${tmp}/download/homeboy" | cut -d' ' -f1)"
grep -Fx "binary-sha256=${expected}" "${tmp}/output" >/dev/null || { printf 'FAIL: selected binary digest was not published\n'; exit 1; }
printf 'PASS: candidate binary selection is newest-attempt deterministic and digest-bound\n'
