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
| `package_identity` | Manifest paths for reading package name, version, and declared dependency maps. |
| `helpers` | Extension-owned helper capabilities that prepare dependencies without requiring core to know their implementation. |

## Discovery

Core consumers should discover manifests through `index.json` from the installed
extension directory. JavaScript consumers can import `index.mjs` and call
`dependencyAdapterManifestPaths()` or `loadDependencyAdapterManifests()` when a
filesystem-backed helper is more convenient than raw JSON loading.

Adapters should not describe:

| Excluded Concern | Owner |
| --- | --- |
| Product-specific package/plugin choices | Caller or product extension. |
| Credential mapping | Caller workflow or runtime profile. |
| Remote execution substrate details | Runner/runtime provider. |
| Core dispatch behavior | Homeboy core.
