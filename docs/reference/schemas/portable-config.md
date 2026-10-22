# Portable Component Config (`homeboy.json`)

A `homeboy.json` file in a repo root defines portable component configuration that travels with the code. Clone a repo, run one command, and homeboy knows how to build, test, version, and deploy it.

## Schema

```json
{
  "id": "string",
  "remote_path": "string",
  "build_artifact": "string",
  "deploy_together": ["component-id"],
  "extract_command": "string",
  "runtimes": { "node": { "version": "24" } },
  "version_targets": [
    {
      "file": "string",
      "pattern": "string",
      "artifact_path": "string"
    }
  ],
  "changelog_target": "string",
  "extensions": {
    "extension_id": {}
  },
  "deployment_provider": {
    "extension": "string",
    "provider": "string",
    "policy": {}
  }
}
```

The component `id` field is required and must be a non-empty string. All other fields are optional. `local_path` is always machine-specific and provided at registration or resolution time.

## Example

```json
{
  "id": "sample-plugin",
  "remote_path": "wp-content/plugins/sample-plugin",
  "version_targets": [
    {
      "file": "sample-plugin.php",
      "pattern": "Version:\\s*([0-9.]+)"
    }
  ],
  "changelog_target": "docs/CHANGELOG.md",
  "extensions": {
    "wordpress": {}
  },
  "deployment_provider": {
    "extension": "hosting-provider",
    "provider": "hosting.deploy",
    "policy": {
      "deployment_mode": "release"
    }
  }
}
```

## Runtime requirements

Declare runtime versions at the top level, for example `"runtimes": { "node": { "version": "24" } }`, with or without an extension. `homeboy component env` reports these with source `"component"`, taking precedence over extension settings, detected versions, and extension requirements. CI setup installs those runtimes.

## Dependency stack edges

`dependency_stack` is an optional array of edges. Each edge requires
`upstream`, `downstream`, and `package` strings. Optional fields are `update`
(command string), `rebuild` (boolean), `post_update` and `test` (command arrays),
and:

| Field | Shape / meaning |
|-------|-----------------|
| `source` | Exactly one of `{ "repo": "git-url", "prefix": "tag-namespace" }`, `{ "github_release": "owner/repo" }`, or `{ "npm": "package-name" }` |
| `lock` | `{ "kind": "version-file", "path": "relative/path" }`; trimmed contents supply the locked semver; refresh writes the version and newline without running providers, installs, or edge commands |
| `policy` | `{ "max": "same-major" }`; newer majors are `held`, left untouched, and counted as unresolvable for exit status |

Without `lock`, existing provider-backed locked-version lookup and refresh
behavior are unchanged. GitHub sources accept stable `v?<semver>` release
tags, excluding drafts and prereleases. npm sources use `dist-tags.latest`.
Source failures are unresolvable, never current. Version-file paths must
resolve within the checkout. See [deps commands](../../commands/deps.md#plain-version-pins)
for complete examples and breaking-verdict behavior.

## Usage

### Initialize or refresh repo config

```bash
# Initialize repo-owned homeboy.json from supported create flags
homeboy component create --local-path /path/to/repo --remote-path wp-content/plugins/my-plugin

# If homeboy.json already exists, preserve unknown fields and refresh known fields
homeboy component create --local-path /path/to/repo

# Override any supported field from the CLI:
homeboy component create --local-path /path/to/repo --changelog-target "CHANGELOG.md"
```

### What stays local (not in homeboy.json)

| Field | Why |
|-------|-----|
| `local_path` | Absolute path, varies per machine |

### What goes in homeboy.json

| Field | Description |
|-------|-------------|
| `id` | Required stable component identifier |
| `remote_path` | Deploy target relative to project `base_path` |
| `build_artifact` | Build output path relative to repo root |
| `deploy_together` | Component IDs that must be deployed in the same operation as this component |
| `extract_command` | Post-upload command (supports `{artifact}`, `{targetDir}`) |
| `version_targets` | Version detection patterns (`file`, `pattern`, optional `artifact_path`) |
| `changelog_target` | Path to changelog file |
| `scripts` | Optional component-owned `lint`, `test`, `build`, `bench`, and `trace` shell commands |
| `deps` | Declarative dependency manifest (`provider`, `commands`, `outputs`, `packages`); see [deps](../../commands/deps.md) |
| `runtimes` | Runtime IDs mapped to version requirements, independent of extensions |
| `extensions` | Extension configuration (e.g., `{"wordpress": {}}`) |
| `capability_extensions` | Optional explicit extension ownership by capability label (e.g., `{"deps": "nodejs"}`) when more than one linked extension supports the same capability |
| `deployment_provider` | Repository-owned provider policy: extension, provider ID, and opaque inline `policy` |

Build, lint, test, bench, trace, and deps behavior resolves from `scripts.<capability>` first, then linked extensions. When multiple linked extensions support the same capability, set `capability_extensions.<capability>` to the owning extension. Use `scripts.build` for component-owned shell builds; component-level `build_command` is not supported.

Use `deploy_together` for coupled components that are versioned or built separately but must stay in sync at runtime. When a deploy selection includes one member of a declared group without the rest, Homeboy fails the plan before building or uploading.

Layered deployment providers keep all repository-owned Homeboy policy inline in root `homeboy.json` under `deployment_provider.policy`. Product, application, and runtime code has no Homeboy dependency and does not need a separate deployment contract file. `deployment_provider.contract` remains available only for legacy unlayered providers. Provider target input is intentionally project-only and never belongs in portable `homeboy.json`. The `homeboy/deployment-provider-payload/v1` policy reference digest is SHA-256 over Homeboy canonical JSON bytes: object keys are recursively sorted, arrays preserve order, and scalar values are unchanged.

## Precedence

CLI flags override `homeboy.json` values. This lets teams share a base config while individuals customize for their environment:

```
homeboy.json (repo)  →  CLI flags (override)  →  ~/.config/homeboy/components/ (stored)
```

## Related

- [Component schema](component-schema.md) - Full component configuration reference
- [Component command](../../commands/component.md) - CLI reference
