#!/usr/bin/env bash
#
# Configure SSH for fleet/deploy commands and optional publishers.
#
# Supports two modes:
#   1. SSH_KEY provided: writes the key, starts ssh-agent, adds known_hosts
#   2. SSH_KEY empty: assumes SSH is already configured (e.g. webfactory/ssh-agent)
#
# Env vars:
#   SSH_KEY         — SSH private key content (optional)
#   SSH_KNOWN_HOSTS — extra known_hosts entries (optional)
#   SSH_REQUIRE_KNOWN_HOSTS — require pinned hosts and strict verification
#
# Outputs (GITHUB_OUTPUT):
#   ssh-configured — true|false

set -euo pipefail

SSH_KEY="${SSH_KEY:-}"
SSH_KNOWN_HOSTS="${SSH_KNOWN_HOSTS:-}"
SSH_REQUIRE_KNOWN_HOSTS="${SSH_REQUIRE_KNOWN_HOSTS:-false}"

SSH_DIR="${HOME}/.ssh"
mkdir -p "${SSH_DIR}"
chmod 700 "${SSH_DIR}"

if [ -n "${SSH_KEY}" ]; then
  echo "Configuring SSH from provided key..."

  if [ "${SSH_REQUIRE_KNOWN_HOSTS}" = "true" ] && [ -z "${SSH_KNOWN_HOSTS}" ]; then
    echo "::error::SSH_KNOWN_HOSTS is required when strict publisher host verification is enabled. Provide pinned host keys; refusing unauthenticated host discovery."
    exit 1
  fi

  # Write the private key
  KEY_FILE="${SSH_DIR}/id_ed25519"
  printf '%s\n' "${SSH_KEY}" > "${KEY_FILE}"
  chmod 600 "${KEY_FILE}"

  # Ensure the key has a trailing newline (some secrets managers strip it)
  if [ "$(tail -c 1 "${KEY_FILE}" | wc -l)" -eq 0 ]; then
    printf '\n' >> "${KEY_FILE}"
  fi

  # Start ssh-agent if not already running
  if [ -z "${SSH_AUTH_SOCK:-}" ]; then
    eval "$(ssh-agent -s)"
    echo "SSH_AUTH_SOCK=${SSH_AUTH_SOCK}" >> "${GITHUB_ENV}"
    echo "SSH_AGENT_PID=${SSH_AGENT_PID}" >> "${GITHUB_ENV}"
    echo "ssh-agent-owned=true" >> "${GITHUB_OUTPUT}"
  else
    echo "ssh-agent-owned=false" >> "${GITHUB_OUTPUT}"
  fi

  # Publish ownership before operations that can fail so the composite action
  # can still remove the key and terminate an agent on an interrupted setup.
  echo "ssh-key-owned=true" >> "${GITHUB_OUTPUT}"
  echo "ssh-key-file=${KEY_FILE}" >> "${GITHUB_OUTPUT}"

  # Add the key to the agent
  ssh-add "${KEY_FILE}"

  # Configure known_hosts. Strict publisher mode accepts only caller-pinned
  # entries; legacy mode retains the existing GitHub convenience behavior.
  KNOWN_HOSTS_FILE="${SSH_DIR}/known_hosts"
  touch "${KNOWN_HOSTS_FILE}"
  chmod 644 "${KNOWN_HOSTS_FILE}"

  if [ "${SSH_REQUIRE_KNOWN_HOSTS}" != "true" ]; then
    # Add GitHub's SSH keys for legacy deploy/fleet callers.
    ssh-keyscan -t ed25519,rsa github.com >> "${KNOWN_HOSTS_FILE}" 2>/dev/null || true
  fi

  # Add user-provided known_hosts entries
  if [ -n "${SSH_KNOWN_HOSTS}" ]; then
    printf '%s\n' "${SSH_KNOWN_HOSTS}" >> "${KNOWN_HOSTS_FILE}"
  fi

  if [ "${SSH_REQUIRE_KNOWN_HOSTS}" = "true" ]; then
    export GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=yes -o UserKnownHostsFile=${KNOWN_HOSTS_FILE}"
    echo "GIT_SSH_COMMAND=${GIT_SSH_COMMAND}" >> "${GITHUB_ENV}"
  fi

  # Configure SSH to accept new host keys for legacy non-GitHub hosts.
  # Strict publisher mode overrides this with GIT_SSH_COMMAND above.
  SSH_CONFIG_FILE="${SSH_DIR}/config"
  if [ ! -f "${SSH_CONFIG_FILE}" ] || ! grep -q "StrictHostKeyChecking" "${SSH_CONFIG_FILE}" 2>/dev/null; then
    cat >> "${SSH_CONFIG_FILE}" << 'SSHCONFIG'

# Added by homeboy-action for fleet/deploy commands
Host *
  StrictHostKeyChecking accept-new
  ServerAliveInterval 60
  ServerAliveCountMax 3
SSHCONFIG
    chmod 600 "${SSH_CONFIG_FILE}"
  fi

  echo "SSH configured: key loaded, agent running, known_hosts populated"
  echo "ssh-configured=true" >> "${GITHUB_OUTPUT}"

elif [ -n "${SSH_AUTH_SOCK:-}" ]; then
  echo "SSH agent already running (SSH_AUTH_SOCK=${SSH_AUTH_SOCK})"

  # Still add user-provided known_hosts if any
  if [ -n "${SSH_KNOWN_HOSTS}" ]; then
    KNOWN_HOSTS_FILE="${SSH_DIR}/known_hosts"
    touch "${KNOWN_HOSTS_FILE}"
    printf '%s\n' "${SSH_KNOWN_HOSTS}" >> "${KNOWN_HOSTS_FILE}"
    echo "Added extra known_hosts entries"
  fi

  echo "ssh-configured=true" >> "${GITHUB_OUTPUT}"
  echo "ssh-key-owned=false" >> "${GITHUB_OUTPUT}"

else
  echo "No SSH key provided and no SSH agent detected"
  echo "Fleet/deploy commands requiring SSH will fail unless SSH is configured by a prior step"
  echo "ssh-configured=false" >> "${GITHUB_OUTPUT}"
  echo "ssh-key-owned=false" >> "${GITHUB_OUTPUT}"
fi
