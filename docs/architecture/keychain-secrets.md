# Keychain and Secrets Management

Homeboy stores sensitive credentials in the OS-native keychain/credential
manager instead of plaintext configuration files.

## Supported Platforms

| Platform | Backend | Service | Account |
| --- | --- | --- | --- |
| macOS | Keychain Access (generic password) | `homeboy` | `<scope>:<variable-name>` |
| Linux | libsecret / gnome-keyring | `homeboy` | `<scope>:<variable-name>` |
| Windows | Credential Manager (generic credential) | `homeboy` | `<scope>:<variable-name>` |

## Secret Consumers

- **Agent-task provider credentials.** Store with
  `homeboy agent-task auth set-keychain`, or map a process environment variable
  with `homeboy agent-task auth map-env`. Trace `--secret-env` resolves through
  the same agent-task secret provider after process env.
- **Runner secret references.** A runner's `secret_env` entries may name a
  keychain secret under the `runner` scope.
- **Trace project secrets.** Trace `--secret-env` also reads values previously
  stored under the project scope (`<project-id>:<variable-name>`). Process env
  and agent-task mappings take precedence.

For CI and headless sessions, prefer exported environment variables or
`source: env` mappings: an OS keychain that needs interactive UI (macOS dark
wake, a locked login keychain, or an SSH session) cannot be read.

## Deleting Secrets

```bash
# macOS
security delete-generic-password -s homeboy -a <scope>:<variable-name>

# Linux
secret-tool clear homeboy <scope>:<variable-name>

# Windows
cmdkey /delete:<scope>:<variable-name>
```

## Troubleshooting

If Homeboy cannot access the keychain, unlock it first:

- macOS: `security unlock-keychain ~/Library/Keychains/login.keychain`
- Linux: ensure the keyring daemon is running and unlocked.
- Windows: ensure the Credential Manager service is running.

## Related

- [SSH key management](./ssh-key-management.md) - SSH key passphrase handling
