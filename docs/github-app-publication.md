# Cook publication with a GitHub App

Configure the global Homeboy config (visible in `homeboy config show`) with:

```json
{
  "github_app": {
    "app_id": 3034937,
    "private_key_path": "/etc/homeboy/github-app.pem",
    "installation_id": 123456
  }
}
```

Omit `installation_id` to resolve the installation from the target repository.
Install the app on each target repository with `contents:write` and
`pull_requests:write`. Store the PEM private key outside checkouts, readable only
by the Homeboy service identity (for example mode 0600). Homeboy reads it at use
time, signs an RS256 JWT, and exchanges it for a repository-scoped installation
token. Neither the key nor token belongs in configuration values or run evidence.
The `jsonwebtoken` dependency provides maintained RS256/PEM support using the
existing ring crypto ecosystem rather than custom RSA/signature code; HTTP and
JSON reuse reqwest and serde_json already used by Homeboy.

Cook uses installation credentials for publication pushes, remote-head checks,
and PR operations. Earlier checkout/base materialization still uses the existing
Git transport configuration; private repositories may still need read credentials
for that phase. This change does not replace all controller Git authentication.
Without `github_app`, existing ambient credentials remain in use. Configured app
authentication errors fail closed rather than silently using the personal login.
Run metadata records `publication_identity` (`github_app:<app id>` or
`ambient_gh`).

Callers may supply `HOMEBOY_REQUESTER`, a single-line opaque identity (up to 512
bytes), alongside `HOMEBOY_CALLER_CONTEXT`. It is captured in `client_context`
before dispatch and preserved across handoffs as run metadata; it is not
rendered in the PR body. No integration-specific identity interpretation is performed.

**Merge safety:** `pull_requests:write` plus `contents:write` can merge through the
API. For operator-only merging, configure branch protection that requires a
review; app permissions alone do not enforce this policy.
