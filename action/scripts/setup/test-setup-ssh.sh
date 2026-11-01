#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

ssh-keygen -q -t ed25519 -N '' -f "${TMP_DIR}/key"
private_key="$(<"${TMP_DIR}/key")"
output_file="${TMP_DIR}/output"
env_file="${TMP_DIR}/env"

if HOME="${TMP_DIR}/home-missing" GITHUB_OUTPUT="${output_file}" GITHUB_ENV="${env_file}" \
  SSH_KEY="${private_key}" SSH_REQUIRE_KNOWN_HOSTS=true \
  bash "${ROOT_DIR}/scripts/setup/setup-ssh.sh" >"${TMP_DIR}/missing.log" 2>&1; then
  printf 'FAIL: strict publisher SSH setup accepted missing host trust\n'
  exit 1
fi
if grep -Fq -- "${private_key}" "${TMP_DIR}/missing.log"; then
  printf 'FAIL: missing-host diagnostic printed private key material\n'
  exit 1
fi
printf 'PASS: strict publisher SSH setup rejects missing pinned host trust\n'

mkdir -p "${TMP_DIR}/home"
GITHUB_OUTPUT="${output_file}" GITHUB_ENV="${env_file}" \
  HOME="${TMP_DIR}/home" SSH_KEY="${private_key}" \
  SSH_KNOWN_HOSTS='example.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakePinnedHostKey' \
  SSH_REQUIRE_KNOWN_HOSTS=true \
  bash "${ROOT_DIR}/scripts/setup/setup-ssh.sh" >"${TMP_DIR}/configured.log" 2>&1

if grep -Fq -- "${private_key}" "${TMP_DIR}/configured.log"; then
  printf 'FAIL: configured SSH setup printed private key material\n'
  exit 1
fi
grep -Fqx 'ssh-key-owned=true' "${output_file}"
grep -Fq 'StrictHostKeyChecking=yes' "${env_file}"
grep -Fq 'UserKnownHostsFile=' "${env_file}"
printf 'PASS: strict publisher SSH setup uses pinned hosts without secret output\n'

SSH_AGENT_OWNED=true \
  SSH_AGENT_PID="$(grep '^SSH_AGENT_PID=' "${env_file}" | cut -d= -f2-)" \
  SSH_AUTH_SOCK="$(grep '^SSH_AUTH_SOCK=' "${env_file}" | cut -d= -f2-)" \
  SSH_KEY_FILE="${TMP_DIR}/home/.ssh/id_ed25519" \
  GITHUB_ENV="${TMP_DIR}/cleanup-env" \
  bash "${ROOT_DIR}/scripts/setup/cleanup-ssh.sh" >"${TMP_DIR}/cleanup.log"

if [ -e "${TMP_DIR}/home/.ssh/id_ed25519" ]; then
  printf 'FAIL: SSH private key file survived cleanup\n'
  exit 1
fi
if grep -Fq -- "${private_key}" "${TMP_DIR}/cleanup.log"; then
  printf 'FAIL: SSH cleanup printed private key material\n'
  exit 1
fi
grep -Fqx 'GIT_SSH_COMMAND=' "${TMP_DIR}/cleanup-env"
printf 'PASS: SSH cleanup removes the key file and emits no secret\n'
