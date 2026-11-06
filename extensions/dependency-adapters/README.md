# Dependency Adapter Manifests

Homeboy Extensions owns concrete dependency materialization knowledge for
ecosystems. Homeboy core can discover and dispatch adapter manifests without
knowing how Node.js, Composer, or WordPress projects install dependencies.

The manifest contract is declarative. It describes adapter capabilities,
project signals, lockfile priority, dependency outputs, and helper surfaces. It
does not execute installs, choose product-specific packages, or embed caller
policy.

## Files

| File | Purpose |
| --- | --- |
| `schema.json` | Stable manifest shape for extension-owned dependency adapters. |
| `index.json` | Stable manifest index for core or runners to discover installed adapter manifests. |
| `index.mjs` | Node helper that loads the manifest index and manifest bodies from this directory. |
| `examples/nodejs.json` | Node.js package manager selection and dependency outputs. |
| `examples/composer.json` | Composer dependency materialization for PHP projects. |
| `examples/wordpress.json` | WordPress helper composition over Composer, Node.js, and WP-CLI surfaces. |

## Boundary

Adapters may describe:

| Field | Meaning |
| --- | --- |
| `ecosystem` | Generic ecosystem identity, such as `nodejs`, `php`, or `wordpress`. |
| `project_signals` | Files that identify a project root or optional capability. |
| `lockfile_priority` | Ordered lockfiles for deterministic package-manager selection. |
| `package_managers` | Selection signals, install intent, script runner commands, and dependency outputs. |
| `package_identity` | Manifest paths for reading package name, version, and declared dependency maps, plus an optional `lockfile` describing where resolved versions are recorded so status reports them without installing. |
| `helpers` | Extension-owned helper capabilities that prepare dependencies without requiring core to know their implementation. |
| `download_cache` | Optional package-manager object declaring the cache environment variable (`env`) for runner-owned shared downloads under gate HOME/XDG isolation. |
| `download_auth` | Optional package-manager object declaring a credential environment variable (`env`), real-HOME-relative `file`, and injection mode (`as`: `content` or `path`). |

## Download cache variables

The bundled declarations follow the tools' configuration documentation:

- Composer: [`COMPOSER_CACHE_DIR`](https://getcomposer.org/doc/03-cli.md#composer-cache-dir)
  overrides `config.cache-dir`, including project-local cache paths.
- npm: [`npm_config_cache`](https://docs.npmjs.com/cli/v11/using-npm/config#environment-variables)
  selects the `cache` setting.
- pnpm: [`npm_config_store_dir`](https://github.com/pnpm/pnpm.io/blob/main/versioned_docs_archived/version-9.x/configuring.md)
  uses npm-style configuration for the
  [`store-dir`](https://github.com/pnpm/pnpm.io/blob/main/versioned_docs_archived/version-9.x/npmrc.md#store-dir) setting.
- Yarn Classic: [`YARN_CACHE_FOLDER`](https://classic.yarnpkg.com/en/docs/cli/cache/)
  selects the download cache directory. This declaration does not promise support
  for Yarn Berry's different cache configuration.

## Download authentication

An optional package-manager `download_auth` object declares an environment
variable (`env`), a nonempty relative credential `file`, and `as: content` or
`as: path`. During isolated gate dependency hydration, core resolves the file
against the runner's real HOME, not the isolated HOME. Content mode injects
UTF-8 file contents; path mode injects the absolute source path without copying
it. Explicit gate environment overrides win, missing files leave automatic auth
unavailable, and core redacts credential values from hydration output.

The bundled declarations follow the tools' configuration documentation:

- Composer: [`COMPOSER_AUTH`](https://getcomposer.org/doc/03-cli.md#composer-auth)
  receives the content of `.config/composer/auth.json`.
- npm: [`NPM_CONFIG_USERCONFIG`](https://docs.npmjs.com/cli/v11/using-npm/config#npmrc-files)
  selects the per-user configuration file, so it receives the path to `.npmrc`.
  This exposes the whole runner-managed configuration file to npm, not just auth.
  Path mode accesses the original file; the tool must not mutate it.

No auth mapping is declared for pnpm or Yarn. Credential provisioning remains
operator-owned. See [core's download cache and credential contract](../../docs/gate-dependency-download-cache.md)
for source validation, overrides, and redaction details.

## Discovery

Core consumers should discover manifests through `index.json` from the installed
extension directory. JavaScript consumers can import `index.mjs` and call
`dependencyAdapterManifestPaths()` or `loadDependencyAdapterManifests()` when a
filesystem-backed helper is more convenient than raw JSON loading.

Adapters should not describe:

| Excluded Concern | Owner |
| --- | --- |
| Product-specific package/plugin choices | Caller or product extension. |
| Credential provisioning and policy | Caller workflow or runtime profile; adapters only declare the tool's input mapping. |
| Remote execution substrate details | Runner/runtime provider. |
| Core dispatch behavior | Homeboy core.
