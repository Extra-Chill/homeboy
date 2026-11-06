# Gate dependency download caches and credentials

Dependency-adapter manifests (`homeboy-extension/dependency-adapter-manifest/v1`)
may opt a package manager into a shared download cache:

```json
{
  "package_managers": [
    {
      "id": "example",
      "download_cache": { "env": "EXAMPLE_DOWNLOAD_CACHE" }
    }
  ]
}
```

The snippet shows only the new fields; existing selection and command declarations
are still required. `env` must be an environment-variable name. The manager ID
must be a single directory name containing ASCII letters, digits, dots, hyphens,
or underscores, and cannot be `.` or `..`.

During dependency hydration with gate HOME or XDG isolation enabled, Homeboy
sets this variable to `<homeboy data>/controller-state/dependency-download-cache/<id>`.
The runner creates private, runner-owned directories, rejects symlink roots and
directories writable by others, and reuses the same directory across gates.
Package managers remain responsible for concurrent cache access. HOME remains
isolated; this mapping does not expose credentials or the operator's home cache.

An explicit `--gate-env` or `--gate-env-from` value for the declared variable wins.
Gate-setup outcomes record the effective mapping in `download_cache`, including
on install failure. Without isolation or without a declaration, behavior is
unchanged. Only hydration subprocesses receive the automatic mapping; it does
not modify the controller's process environment.

The separately maintained `homeboy-extensions` repository needs a follow-up:
add `"download_cache": { "env": "COMPOSER_CACHE_DIR" }` to the composer
`package_managers` entry, and `"download_cache": { "env": "npm_config_cache" }`
to the npm entry. No package-manager-specific mapping is built into Homeboy.

## Declared download credentials

A package-manager entry may also declare:

```json
"download_auth": { "env": "EXAMPLE_DOWNLOAD_AUTH", "file": ".config/example/auth.json", "as": "content" }
```

Like `download_cache`, this applies only during isolated gate dependency
hydration (including reusable-state assessment). The source resolves against
the runner's real HOME, before subprocess HOME/XDG isolation, not the gate HOME.
`as: content` injects UTF-8 file contents; `as: path` injects the absolute source
path. Homeboy never copies the file into the gate HOME. Path mode gives the tool
access to the original file, not a read-only copy; tools must not mutate it.

`env` must be an environment-variable name. `file` must be a nonempty relative
path with only normal components (no absolute paths, `..`, or symlinks), naming
a regular file owned by the runner user. Invalid or unreadable sources fail
closed with a generic error, without file contents. A missing file is not an
error: hydration proceeds without the automatic mapping and records
`download_auth: { "EXAMPLE_DOWNLOAD_AUTH": "unavailable" }`.

Explicit `--gate-env` / `--gate-env-from` values win, even if the declared file
is missing. Evidence records only the env name and absolute source path, or
`operator_override`. Credential values are registered with the hydration's
secret-aware redaction policy: command stdout/stderr, structured progress, and
serialized outcome strings are scrubbed. File contents (including JSON string
values) are treated as secrets in both modes. Operator overrides are also secret.
No process-global environment or redaction state is modified.

### homeboy-extensions follow-up

Add these exact fields to the respective `package_managers` entries (alongside
the cache declarations above):

- composer: `"download_auth": { "env": "COMPOSER_AUTH", "file": ".config/composer/auth.json", "as": "content" }`
- npm: `"download_auth": { "env": "NPM_CONFIG_USERCONFIG", "file": ".npmrc", "as": "path" }`

The npm mapping is appropriate when the runner's `.npmrc` carries download
authentication. It exposes the whole declared configuration to npm; use only a
runner-managed file intended for dependency setup.

Related: #17455, #17347 and #17261.
