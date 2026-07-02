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
| `examples/nodejs.json` | Node.js package manager selection and dependency outputs. |
| `examples/composer.json` | Composer dependency materialization for PHP projects. |
| `examples/wordpress.json` | WordPress helper composition over Composer, Node.js, and WP-CLI surfaces. |

## Boundary

Adapters may describe:

| Field | Meaning |
| --- | --- |
| `ecosystem` | Generic ecosystem identity, such as `nodejs`, `php`, or `wordpress`. |
| `project_signals` | Files that identify a project root or optional capability. |
| `package_managers` | Selection signals, install intent, script runner commands, and dependency outputs. |
| `helpers` | Extension-owned helper capabilities that prepare dependencies without requiring core to know their implementation. |

Adapters should not describe:

| Excluded Concern | Owner |
| --- | --- |
| Product-specific package/plugin choices | Caller or product extension. |
| Credential mapping | Caller workflow or runtime profile. |
| Remote execution substrate details | Runner/runtime provider. |
| Core dispatch behavior | Homeboy core.
