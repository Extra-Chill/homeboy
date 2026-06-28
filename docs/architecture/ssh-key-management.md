# SSH Key Management

Homeboy manages SSH keys for remote server access, including keychain integration for passphrase management.

## Overview

SSH keys in Homeboy are stored as file references in server configurations (`identity_file` field) with passphrases stored securely in the OS keychain.

## Key Locations

SSH keys are stored on the local filesystem, not in Homeboy configuration:

**Standard locations:**
- **macOS/Linux**: `~/.ssh/id_rsa`, `~/.ssh/id_ed25519`, `~/.ssh/id_ecdsa`
- **Windows**: `%USERPROFILE%\.ssh\id_rsa`, `%USERPROFILE%\.ssh\id_ed25519`

Homeboy references these keys via absolute paths in server configuration.

## Server Configuration

SSH key configuration is part of server definition in `servers/<server_id>.json`:

```json
{
  "id": "production",
  "name": "Production Server",
  "host": "example.com",
  "port": 22,
  "user": "deploy",
  "identity_file": "/Users/dev/.ssh/id_ed25519",
  "forward_agent": true
}
```

### Fields

- **`identity_file`** (string): Absolute path to SSH private key
- **`forward_agent`** (boolean): Enable SSH agent forwarding

## Key Generation

Generate a new SSH key pair:

```bash
# Generate ED25519 key (recommended)
ssh-keygen -t ed25519 -C "your@email.com"

# Generate RSA key (legacy compatibility)
ssh-keygen -t rsa -b 4096 -C "your@email.com"
```

**During generation:**
1. Accept default location or specify custom path
2. Enter a strong passphrase (recommended)
3. Confirm passphrase

The passphrase is stored in keychain on first use.

## Key Deployment

### Copy Public Key to Server

```bash
# Using ssh-copy-id (recommended)
ssh-copy-id user@hostname

# Manual copy
cat ~/.ssh/id_ed25519.pub | ssh user@hostname "mkdir -p ~/.ssh && cat >> ~/.ssh/authorized_keys"
```

### Add to Server Configuration

```bash
homeboy server create production \
  --host example.com \
  --user deploy \
  --identity-file ~/.ssh/id_ed25519
```

## Passphrase Management

### Initial Storage

When you first use an SSH key with Homeboy, the passphrase is cached in the OS keychain:

```bash
homeboy ssh <project_id>
```

If the key has a passphrase, Homeboy:
1. Prompts for passphrase
2. Stores passphrase in keychain
3. Uses keychain for subsequent connections

### Passphrase Retrieval

Passphrases are automatically retrieved from keychain when:
- Establishing SSH connections via `homeboy ssh`
- Running remote commands via SSH
- Uploading files to remote servers

No manual passphrase entry is required after initial storage.

### Passphrase Updates

To change an SSH key's passphrase:

```bash
ssh-keygen -p -f ~/.ssh/id_ed25519
```

This updates the key file. Homeboy will detect the new passphrase on next use and update the keychain entry.

### Delete Cached Passphrase

Remove cached passphrase from keychain:

**macOS:**
```bash
security delete-generic-password -s homeboy -a ssh_key_<identity_file_hash>
```

**Linux:**
```bash
secret-tool clear homeboy ssh_key_<identity_file_hash>
```

**Windows:**
```bash
cmdkey /delete:ssh_key_<identity_file_hash>
```

Note: The exact key name depends on the key file path hash.

## SSH Agent Forwarding

When `forward_agent` is enabled in server configuration, SSH keys from your local agent are forwarded to the remote server.

### Benefits

- Remote servers can use your local SSH keys
- Enables Git operations on remote server using your credentials
- Reduces need for keys on remote servers

### Configuration

```bash
homeboy server set production --forward-agent true
```

### Security Considerations

SSH agent forwarding grants the remote server access to your forwarded SSH identities. Enable only on trusted servers.

## Key Types

### ED25519 (Recommended)

Modern, secure, and fast:
```bash
ssh-keygen -t ed25519 -C "your@email.com"
```

### RSA (Legacy)

For compatibility with older systems:
```bash
ssh-keygen -t rsa -b 4096 -C "your@email.com"
```

### ECDSA

Alternative to RSA:
```bash
ssh-keygen -t ecdsa -b 521 -C "your@email.com"
```

## Troubleshooting

### Key Permission Errors

SSH keys must have restrictive permissions:

```bash
chmod 600 ~/.ssh/id_ed25519
chmod 644 ~/.ssh/id_ed25519.pub
```

### Passphrase Prompt Loop

If Homeboy repeatedly prompts for passphrase:
1. Verify key file path in server configuration is correct
2. Delete cached passphrase from keychain
3. Test key passphrase: `ssh -i ~/.ssh/id_ed25519 user@hostname`

### Connection Refused

Check:
- Server is accessible: `ping hostname`
- SSH port is correct (default: 22)
- Firewall allows SSH connections
- User has SSH access on remote server

### Agent Forwarding Not Working

If agent forwarding fails:
1. Verify SSH agent is running: `ps aux | grep ssh-agent`
2. Add keys to agent: `ssh-add`
3. Verify server configuration has `forward_agent: true`

## Best Practices

1. **Use ED25519 keys**: More secure and faster than RSA
2. **Always use passphrases**: Protect keys with strong passphrases
3. **Separate keys per server**: Use different keys for different environments
4. **Rotate keys regularly**: Generate new keys periodically
5. **Disable password authentication**: Configure servers to only allow key-based authentication
6. **Limit agent forwarding**: Enable only on trusted servers
7. **Back up keys**: Store private key backups in secure, offline storage

## Related

- [Server command](../commands/server.md) - Manage server configuration
- [SSH command](../commands/ssh.md) - Remote shell access
- [Server schema](../reference/schemas/server-schema.md) - Server configuration structure
- [Keychain/secrets management](./keychain-secrets.md) - Passphrase storage
