# Keychain and Secrets Management

Homeboy securely stores sensitive credentials using OS-native keychain/credential manager systems.

## Overview

Homeboy never stores secrets in plaintext configuration files. All sensitive data is encrypted and stored in the OS keychain:

- **API tokens** for project authentication
- **Database passwords** for remote database access
- **SSH key passphrases** for encrypted SSH keys

## Supported Platforms

### macOS

Uses **Keychain Access**:
- Service: `homeboy`
- Account: `<key_name>` (for API variables: `<project-id>:<variable-name>`)
- Type: Generic password

### Linux

Uses **libsecret** or **gnome-keyring**:
- Service: `homeboy`
- Account: `<key_name>`
- Type: Generic secret

### Windows

Uses **Windows Credential Manager**:
- Application name: `homeboy`
- Target: `<key_name>`
- Type: Generic credential

## Secret Types

### API Tokens

Stored for project API authentication.

**Key naming:** `<project-id>:<variable-name>`

**Usage:**
```bash
# Store API token
homeboy auth set --project <project_id> token

# Token is automatically retrieved during API requests
homeboy api <project_id> get /posts
```

### Database Passwords

Stored for remote database connections.

**Key naming:** `homeboy_db_<project_id>`

**Usage:**
- Configure database in project JSON without password field
- Homeboy retrieves password from keychain during database operations

```bash
# Database operations use keychain password automatically
homeboy db <project_id> query "SELECT * FROM posts"
```

### SSH Key Passphrases

Stored for encrypted SSH private keys.

**Key naming:** Derived from SSH key identity file path

**Usage:**
- Passphrases are retrieved automatically when establishing SSH connections
- No need to re-enter passphrase for each connection

## Storing Secrets

### API Tokens

```bash
homeboy auth set --project <project_id> token
```

Prompts for authentication token which is stored securely.

### Database Passwords

Database passwords are stored via project configuration:

```bash
homeboy project set <project_id> --json '{"database": {"password": "<your_password>"}}'
```

Password is extracted and stored in keychain; the project JSON stores only database connection details.

### SSH Key Passphrases

SSH key passphrases are stored automatically when keys are first used. The passphrase is cached in keychain for future use.

## Retrieving Secrets

Secrets are retrieved automatically when needed:

1. **API requests**: Token retrieved before making HTTP request
2. **Database operations**: Password retrieved before connecting
3. **SSH connections**: Passphrase retrieved before authentication

No manual retrieval is required.

## Updating Secrets

### Update API Token

```bash
homeboy auth set --project <project_id> token
```

Re-running the auth command prompts for new token and updates the keychain entry.

### Update Database Password

```bash
homeboy project set <project_id> --json '{"database": {"password": "<new_password>"}}'
```

The new password replaces the existing keychain entry.

### Update SSH Key Passphrase

SSH key passphrases cannot be updated directly. If you change a key's passphrase, Homeboy will prompt for the new passphrase on next use and update the keychain entry.

## Deleting Secrets

### Delete API Token

```bash
# macOS
security delete-generic-password -s homeboy -a <project_id>:token

# Linux
secret-tool clear homeboy <project_id>:token

# Windows
cmdkey /delete:<project_id>:token
```

### Delete Database Password

```bash
# macOS
security delete-generic-password -s homeboy -a homeboy_db_<project_id>

# Linux
secret-tool clear homeboy homeboy_db_<project_id>

# Windows
cmdkey /delete:homeboy_db_<project_id>
```

### Delete SSH Key Passphrase

SSH key passphrases use derived key names based on key file path. Delete via platform-specific commands referencing the key file.

## Security Best Practices

1. **Never store secrets in JSON files**: Always use Homeboy's keychain integration
2. **Use strong, unique passwords**: Different credentials for each project
3. **Rotate credentials regularly**: Update tokens and passwords periodically
4. **Use environment-specific credentials**: Separate credentials for development, staging, and production
5. **Enable keychain lock**: Ensure OS keychain is locked when not in use
6. **Use two-factor authentication**: When supported by APIs
7. **Audit access**: Periodically review which projects have stored credentials

## Troubleshooting

### Keychain Not Unlocked

If Homeboy cannot access the keychain, unlock it first:

**macOS:**
```bash
security unlock-keychain ~/Library/Keychains/login.keychain
```

**Linux:**
Ensure the keyring daemon is running and unlocked.

**Windows:**
Ensure Credential Manager service is running.

### Secret Not Found

If Homeboy reports a secret not found:

1. Verify the project ID is correct
2. Re-run the auth command to store the secret
3. Check the keychain for existing entries using platform-specific commands

### Wrong Secret Retrieved

If Homeboy retrieves incorrect credentials:

1. Delete the keychain entry manually
2. Re-store the secret using Homeboy commands
3. Verify project IDs are not colliding

## Related

- [Auth command](../commands/auth.md) - Store API authentication
- [API client system](./api-client.md) - How API authentication works
- [SSH key management](./ssh-key-management.md) - SSH key passphrase handling
