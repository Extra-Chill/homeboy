#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKFLOW="${ROOT_DIR}/.github/workflows/ci.yml"

if [ "$(grep -c '^      cargo-cache-key: \${{ steps.cargo-cache-key.outputs.key }}$' "${WORKFLOW}")" -ne 1 ] \
  || [ "$(grep -c '^        id: cargo-cache-key$' "${WORKFLOW}")" -ne 1 ] \
  || [ "$(grep -c '^          key: \${{ steps.cargo-cache-key.outputs.key }}$' "${WORKFLOW}")" -ne 1 ]; then
  printf 'FAIL: binary job does not publish and consume one candidate cargo cache identity\n'
  exit 1
fi

if [ "$(grep -c '^      - name: Restore candidate cargo cache$' "${WORKFLOW}")" -ne 2 ] \
  || [ "$(grep -Fc "        if: needs.binary.outputs.cargo-cache-key != ''" "${WORKFLOW}")" -ne 2 ] \
  || [ "$(grep -c '^          key: \${{ needs.binary.outputs.cargo-cache-key }}$' "${WORKFLOW}")" -ne 2 ]; then
  printf 'FAIL: candidate and baseline jobs do not restore the binary job cargo cache identity\n'
  exit 1
fi

if [ "$(grep -c "if: hashFiles('Cargo.lock') != ''" "${WORKFLOW}")" -ne 1 ]; then
  printf 'FAIL: Cargo cache discovery must remain conditional and owned by the candidate checkout\n'
  exit 1
fi

printf 'PASS: reusable differential phases restore the candidate build cargo cache identity\n'
