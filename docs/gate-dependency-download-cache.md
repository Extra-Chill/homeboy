# Gate dependency download caches

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

Related: #17347 and #17261.
