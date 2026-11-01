#!/usr/bin/env bash

set -euo pipefail

SSH_AGENT_OWNED="${SSH_AGENT_OWNED:-false}"
SSH_AGENT_PID="${SSH_AGENT_PID:-}"
SSH_AUTH_SOCK="${SSH_AUTH_SOCK:-}"
SSH_KEY_FILE="${SSH_KEY_FILE:-${HOME}/.ssh/id_ed25519}"

if [ -n "${SSH_AUTH_SOCK}" ] && [ -f "${SSH_KEY_FILE}" ]; then
  ssh-add -d "${SSH_KEY_FILE}" >/dev/null 2>&1 || true
fi

if [ "${SSH_AGENT_OWNED}" = "true" ] && [ -n "${SSH_AGENT_PID}" ] && [ -n "${SSH_AUTH_SOCK}" ]; then
  SSH_AGENT_PID="${SSH_AGENT_PID}" SSH_AUTH_SOCK="${SSH_AUTH_SOCK}" ssh-agent -k >/dev/null 2>&1 || true
fi

rm -f -- "${SSH_KEY_FILE}"

if [ -n "${GITHUB_ENV:-}" ]; then
  printf 'SSH_AGENT_PID=\nSSH_AUTH_SOCK=\nGIT_SSH_COMMAND=\n' >> "${GITHUB_ENV}"
fi

printf 'SSH credentials cleaned up without printing key material.\n'
