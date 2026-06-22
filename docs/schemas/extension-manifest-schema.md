# Extension Manifest Schema

Extension manifests define extension metadata, runtime behavior, platform behaviors, and integration points. Stored as `<extension_id>/<extension_id>.json` in the extension directory.

## Schema

```json
{
  "name": "string",
  "id": "string",
  "version": "string",
  "description": "string",
  "provides": {},
  "scripts": {},
  "audit": {},
  "deploy": {},
  "executable": {},
  "platform": {},
  "structured_sidecars": {},
  "commands": {},
  "actions": [],
  "hooks": {},
  "docs": [],
  "capabilities": [],
  "storage_backend": "string"
}
```

## Fields

### Required Fields

- **`name`** (string): Human-readable extension name
- **`id`** (string): Unique extension identifier (must match directory name)
- **`version`** (string): Extension version (semantic versioning)

### Optional Fields

- **`description`** (string): Extension description
- **`provides`** (object): File extensions and capabilities this extension handles
- **`scripts`** (object): Scripts that implement extension capabilities (fingerprint, refactor)
- **`audit`** (object): Docs audit config — ignore patterns, feature detection, test mapping
- **`deploy`** (object): Deploy lifecycle — verifications, overrides, version patterns
- **`executable`** (object): Standalone tool runtime, inputs, output schema
- **`platform`** (object): Platform behavior definitions (database, deployment, version patterns)
- **`structured_sidecars`** (object): Declares public machine-readable run-directory sidecar contracts
- **`fuzz`** (object): Declares fuzz workload metadata, optional runner script, and optional campaign portability metadata
- **`commands`** (object): Additional CLI commands provided by extension
- **`actions`** (array): Action definitions for `homeboy extension action`; release actions are normal actions whose IDs start with `release.`
- **`hooks`** (object): Lifecycle hooks (pre/post version bump, deploy, release)
- **`docs`** (array): Documentation topic paths
- **`capabilities`** (array): Capabilities provided by extension (e.g., `["storage"]`)
- **`storage_backend`** (string): Storage backend identifier for storage capability

## Provides Configuration

Declares what file types and capabilities this extension handles. Used by the audit system to route files to the correct extension for fingerprinting.

```json
{
  "provides": {
    "file_extensions": ["php", "inc"],
    "capabilities": ["fingerprint", "refactor"],
    "discovery_markers": [
      { "all": ["style.css", "functions.php"] },
      { "all": ["package.json"], "any": ["src/**/*.ts", "*.ts"] }
    ]
  }
}
```

### Provides Fields

- **`file_extensions`** (array): File extensions this extension can process (e.g., `["php", "inc"]`, `["rs"]`)
- **`capabilities`** (array): Capabilities this extension supports (e.g., `["fingerprint", "refactor"]`)
- **`discovery_markers`** (array): Component-root marker rules used by diagnostics and gap reporting to suggest an extension without core knowing ecosystem-specific files.

Each `discovery_markers` rule supports:

- **`all`** (array): Relative marker paths/globs that must all exist.
- **`any`** (array): Relative marker paths/globs where at least one must exist when supplied.

Core treats marker strings generically. Exact strings are checked as paths relative to the component root; strings containing `*`, `?`, or `[` are evaluated as globs relative to the component root.

## Grammar Fingerprint Metadata Contract

Extension-owned `grammar.toml` files may declare fingerprint metadata consumed by Homeboy's generic fingerprint engine. Language and framework semantics belong here, not in core.

Path-derived namespaces are declared with `fingerprint.namespace_derivation`:

```toml
[fingerprint.namespace_derivation]
prefix = "crate::"
strip_leading_segments = 1
separator = "::"
include_file_stem_when_root = true
```

### Namespace Derivation Fields

- **`prefix`** (string): Optional prefix prepended to the derived namespace.
- **`strip_leading_segments`** (integer): Number of leading path segments to remove before deriving the namespace.
- **`separator`** (string): Separator used to join remaining namespace segments. Defaults to `::`.
- **`include_file_stem_when_root`** (boolean): Whether a root-level source file contributes its file stem as the namespace.

If an extension needs path-derived namespaces, it must ship this grammar metadata. Core does not provide language-specific fallbacks.

## Scripts Configuration

Scripts that implement extension capabilities. Each script path is relative to the extension directory.

```json
{
  "scripts": {
    "fingerprint": "scripts/fingerprint.sh",
    "refactor": "scripts/refactor.sh"
  }
}
```

### Scripts Fields

- **`fingerprint`** (string): Script that extracts structural fingerprints from source files. Receives file content on stdin, outputs `FileFingerprint` JSON on stdout.
- **`refactor`** (string): Script that applies refactoring edits to source files. Receives edit instructions on stdin, outputs transformed content on stdout.

## Deploy Configuration

Deploy configuration declares extension-owned deploy behavior. The `deploy` object is an explicit typed contract: unknown nested deploy keys are rejected instead of being preserved as passive metadata.

```json
{
  "deploy": {
    "archive_install": [
      {
        "path_pattern": "/wp-content/plugins/",
        "staging_path": "/tmp/homeboy-wordpress-plugin-staging",
        "root_must_match_target_basename": true,
        "required_header": {
          "file_glob": "*.php",
          "contains": "Plugin Name:"
        },
        "skip_permissions_fix": true
      }
    ]
  }
}
```

### Archive Install Fields

- **`deploy.archive_install[]`** (array): Core-backed archive install policies for deploy targets matched by `path_pattern`. Homeboy copies the artifact to `staging_path`, validates the archive root when requested, replaces the target directory, verifies the required header when configured, and removes the staged artifact after verification.
- **`path_pattern`** (string): Substring matched against the resolved remote target path. The first matching policy becomes the deploy override and verification path for that target.
- **`staging_path`** (string): Remote directory used for the staged archive artifact. Defaults to `/tmp/homeboy-staging`.
- **`root_must_match_target_basename`** (boolean): When true, the first archive root directory must equal the target directory basename before the target is replaced.
- **`required_header`** (object): Optional post-install verification for the installed header file. If present, it must declare exactly one selector: `file` or `file_glob`.
- **`required_header.file`** (string): Exact header file path relative to the archive root. Supports normal deploy template variables such as `{{targetBasename}}`.
- **`required_header.file_glob`** (string): Header file basename glob relative to the archive root, for example `*.php`.
- **`required_header.contains`** (string): Literal text that must exist in the installed header file.
- **`skip_permissions_fix`** (boolean): Skip the normal post-install permissions fix for this target.

`deploy.overrides` remains the lower-level escape hatch for extensions that need a fully custom install command. Archive-shaped plugin or theme installs should prefer `deploy.archive_install` so target basename validation and header verification stay on the shared core path.

## Structured Sidecar Declarations

Extensions can declare which structured run-directory sidecars they emit. `structured_sidecars` is the only manifest field that declares sidecar contracts. Declarations are explicit contracts: if an entry is missing or set to `false`, the extension has not declared that structured sidecar.

```json
{
  "structured_sidecars": {
    "findings": {
      "path": "findings.json",
      "schema_version": "1"
    },
    "producer.summary": {
      "path": "producer-summary.json",
      "schema_version": "1"
    },
    "lint.findings": true,
    "test.coverage": false
  }
}
```

### Sidecar Fields

- **`structured_sidecars.<name>`** (boolean or object): Stable logical sidecar name, for example `findings`, `producer.summary`, `lint.findings`, or `annotations`. `true` declares the sidecar with its default run-dir path. `false` explicitly leaves it undeclared.
- **`structured_sidecars.<name>.enabled`** (boolean): Optional object form enablement flag. Defaults to `true`.
- **`structured_sidecars.<name>.path`** (string): Optional run-directory relative sidecar file or directory path. Known names such as `lint.findings`, `test.results`, `test.failures`, `bench.results`, and `annotations` have default paths.
- **`structured_sidecars.<name>.schema_version`** (string): Optional version of the sidecar payload contract.

The generic `findings` and `producer.summary` names are the preferred contracts for normalized finding output and producer summaries. Legacy producer-specific names such as `lint.findings` can be declared during migration, but they use the same top-level declaration shape. Schema versions are read only from `structured_sidecars.<name>.schema_version`; nested producer fields such as `lint.findings_schema_version` do not declare sidecar contracts.

Known sidecar names default to these run-directory paths when `path` is omitted:

| Name | Default path |
| --- | --- |
| `findings` | `findings.json` |
| `producer.summary` | `producer-summary.json` |
| `lint.findings` | `lint-findings.json` |
| `lint.producers` | `lint-producers.json` |
| `test.results` | `test-results.json` |
| `test.failures` | `test-failures.json` |
| `test.coverage` | `coverage.json` |
| `bench.results` | `bench-results.json` |
| `trace.results` | `trace.json` |
| `resource.summary` | `resource-summary.json` |
| `annotations` | `annotations` |

### Inspection Behavior

Core exposes declared sidecars through manifest inspection, including `homeboy extension show <id>` JSON output. Consumers that need machine-readable output should require the matching declaration before relying on a sidecar.

## Fuzz Capability

Extensions declare fuzz workload metadata with a top-level `fuzz` capability block. The block is first-class manifest support even when it only declares workloads. Execution remains opt-in: `extension_script` must be present before Homeboy can run a fuzz workload through an extension runner.

```json
{
  "fuzz": {
    "workloads": [
      {
        "id": "parser",
        "label": "Parser fuzz",
        "description": "Parser corpus and generated input workload"
      }
    ]
  }
}
```

### Fuzz Fields

- **`fuzz.extension_script`** (string): Optional script path, relative to the extension directory, that executes a selected fuzz workload. Omit this for manifest-only workload discovery.
- **`fuzz.workloads`** (array): Declarative workload entries surfaced by `homeboy fuzz list` and downstream rig/workload tooling.
- **`fuzz.workloads[].id`** (string): Stable workload identifier.
- **`fuzz.workloads[].label`** (string): Optional human-readable workload label.
- **`fuzz.workloads[].description`** (string): Optional workload description.
- **`fuzz.case_artifact`** (string): Optional generic artifact id or semantic key for the primary replayable case artifact.
- **`fuzz.corpus_artifacts`** (array): Optional generic artifact ids or semantic keys for persisted corpus artifacts.
- **`fuzz.seed`** (string): Optional default seed surfaced in `fuzz run` output when the caller did not pass `--seed`.
- **`fuzz.replay_command`** (string): Optional extension-owned command template for replaying a persisted case. Homeboy surfaces this metadata but does not execute `fuzz replay` yet.
- **`fuzz.minimize_command`** (string): Optional extension-owned command template for minimizing a persisted case.
- **`fuzz.result_schema`** (string): Optional campaign result schema identifier. Defaults to `homeboy/fuzz-campaign/v1` in `fuzz run` output.
- **`fuzz.artifact_retention`** (string): Optional retention policy label for campaign artifacts.
- **`fuzz.workloads[].lifecycle`** (object): Optional `homeboy/lifecycle-contract/v1` declaration with `prepare`, `seed`, `snapshot`, `reset`, `rollback`, and `teardown` phases. Extension hooks implement the runtime behavior; Homeboy core owns the contract and result metadata shape.

## Audit Configuration

Configuration for documentation-reference analysis, feature detection, and structural test coverage analysis.

```json
{
  "audit": {
    "ignore_claim_patterns": ["/wp-json/**", "*.min.js"],
    "feature_patterns": ["register_post_type\\(\\s*['\"]([^'\"]+)['\"]"],
    "feature_labels": {
      "register_post_type": "Post Types",
      "register_rest_route": "REST API Routes"
    },
    "doc_targets": {
      "Post Types": {
        "file": "api-reference.md",
        "heading": "## Post Types"
      }
    },
    "feature_context": {
      "register_post_type": {
        "doc_comment": true,
        "block_fields": true
      }
    },
    "test_mapping": {
      "source_dirs": ["src"],
      "test_dirs": ["tests"],
      "test_file_pattern": "tests/{dir}/{name}_test.{ext}",
      "method_prefix": "test_",
      "inline_tests": true,
      "critical_patterns": ["src/core/"]
    }
  }
}
```

### Audit Fields

- **`ignore_claim_patterns`** (array): Glob patterns for paths to ignore during documentation-reference analysis
- **`feature_patterns`** (array): Regex patterns to detect features in source code (must have a capture group for the feature name)
- **`feature_labels`** (object): Maps pattern substrings to human-readable labels for grouping
- **`doc_targets`** (object): Maps feature labels to documentation file paths and headings
- **`feature_context`** (object): Context extraction rules per feature pattern (doc comments, block fields)
- **`test_mapping`** (object): Test coverage mapping convention

### Test Mapping Fields

- **`source_dirs`** (array): Source directories to scan (e.g., `["src"]`, `["inc"]`)
- **`test_dirs`** (array): Test directories to scan (e.g., `["tests"]`)
- **`test_file_pattern`** (string): How source paths map to test paths. Variables: `{dir}`, `{name}`, `{ext}`
- **`method_prefix`** (string): Prefix for test method names (default: `"test_"`)
- **`inline_tests`** (boolean): Whether the language uses inline tests (e.g., Rust `#[cfg(test)]`)
- **`critical_patterns`** (array): Directory patterns that indicate high-priority test coverage (get `Warning` severity instead of `Info`)

## Test Drift Configuration

Changed-test and drift workflows use the canonical `test.drift` contract. `audit.test_mapping` is only for structural test coverage checks and does not declare drift behavior.

```json
{
  "test": {
    "extension_script": "scripts/test.sh",
    "drift": {
      "source_dirs": ["src"],
      "test_dirs": ["tests"],
      "file_extensions": ["rs"],
      "inline_tests": true
    }
  }
}
```

### Test Drift Fields

- **`source_dirs`** (array): Source directories to scan relative to the component root
- **`test_dirs`** (array): Test directories to scan relative to the component root
- **`file_extensions`** (array): File extensions to include when building source/test glob patterns
- **`inline_tests`** (boolean): Whether the language supports inline tests

## Executable Runtime Configuration

Executable runtime configuration defines how executable extensions are executed.

```json
{
  "executable": {
    "runtime": {
      "run_command": "string",
      "setup_command": "string",
      "ready_check": "string",
      "entrypoint": "string",
      "env": {}
    }
  }
}
```

### Runtime Fields

- **`run_command`** (string): Shell command to execute the extension
  - Template variables: `{{extensionPath}}`, `{{entrypoint}}`, `{{args}}`, plus project context variables
  - Example: `"./venv/bin/python3 {{entrypoint}} {{args}}"`
- **`setup_command`** (string): Command to run during install/update (optional)
  - Example: `"python3 -m venv venv && ./venv/bin/pip install -r requirements.txt"`
- **`ready_check`** (string): Command to verify extension readiness (optional)
  - Exit code 0 = ready, non-zero = not ready
  - Example: `"test -f ./venv/bin/python3"`
- **`entrypoint`** (string): Extension entrypoint script (optional)
  - Example: `"main.py"`
- **`env`** (object): Environment variables to set during execution
  - Values can use template variables
  - Example: `{"MY_VAR": "{{extensionPath}}/data"}`

## Runtime Requirements

Extension manifests and `component_env.detect_script` output can declare runtime requirements with a generic `runtimes` map:

```json
{
  "runtime": {
    "runtimes": {
      "php": { "version": "8.2" },
      "node": { "version": "22" }
    }
  }
}
```

Detector output uses the same shape without the outer `runtime` field:

```json
{
  "runtimes": {
    "python": { "version": "3.12" }
  }
}
```

Runtime IDs are extension-owned strings. Runtime requirements must use the canonical `runtimes` map with object values containing `version`; top-level runtime IDs and string shorthand values are invalid.

## Platform Configuration

Platform configuration defines database, deployment, and version detection behaviors.

```json
{
  "platform": {
    "database": {},
    "deployment": {},
    "version_patterns": []
  }
}
```

### Database Configuration

```json
{
  "platform": {
    "database": {
      "cli": {
        "connect": "string",
        "query": "string",
        "tables": "string",
        "describe": "string"
      },
      "defaults": {
        "host": "string",
        "port": number,
        "user": "string"
      }
    }
  }
}
```

#### Database Fields

- **`cli`** (object): Database CLI template commands
  - **`connect`** (string): Connection command template
    - Template variables: `{{db_host}}`, `{{db_port}}`, `{{db_name}}`, `{{db_user}}`
  - **`query`** (string): Query command template
    - Template variables: `{{query}}`, `{{db_host}}`, `{{db_name}}`, etc.
  - **`tables`** (string): List tables command template
  - **`describe`** (string): Describe table command template
- **`defaults`** (object): Default database connection values
  - **`host`** (string): Default host
  - **`port`** (number): Default port
  - **`user`** (string): Default user

### Deployment Configuration

```json
{
  "platform": {
    "deployment": {
      "override_command": "string",
      "override_extract_command": "string"
    }
  }
}
```

#### Deployment Fields

- **`override_command`** (string): Custom build command template
  - Template variables: `{{targetDir}}`, `{{siteRoot}}`, `{{domain}}`, `{{cliPath}}`, `{{allowRootFlag}}`
- **`override_extract_command`** (string): Custom extract command template
  - Template variables: `{{artifact}}`, `{{targetDir}}`, `{{stagingArtifact}}`

### Version Patterns

```json
{
  "platform": {
    "version_patterns": [
      {
        "file": "string",
        "pattern": "string"
      }
    ]
  }
}
```

#### Version Pattern Fields

- **`file`** (string): Path to version file (relative to component root)
- **`pattern`** (string): Regex pattern to extract version

## Commands Configuration

Extensions can register additional top-level CLI commands.

```json
{
  "commands": {
    "<command_name>": {
      "description": "string",
      "run_command": "string",
      "help": "string"
    }
  }
}
```

### Command Fields

- **`description`** (string): Command description for help text
- **`run_command`** (string): Execution template
  - Template variables: `{{args}}`, plus extension runtime variables
- **`help`** (string): Detailed help text (optional)

## Actions Configuration

Actions define executable operations accessible via `homeboy extension action`.
They are an array of typed action objects.

```json
{
  "actions": [
    {
      "id": "sync",
      "label": "Sync data",
      "type": "command",
      "command": "scripts/sync.sh"
    }
  ]
}
```

### Action Fields

- **`id`** (string): Stable action identifier
- **`label`** (string): Human-readable action label
- **`type`** (string): `"command"`, `"api"`, or `"builtin"`
- **`command`** (string): Command to execute for `command` actions
- **`endpoint`** (string): Endpoint for `api` actions
- **`method`** (string): HTTP method for `api` actions
- **`payload`** (object): Optional payload template for `api` actions
- **`builtin`** (string): Legacy UI action type for `builtin` actions

#### Command Action

```json
{
  "actions": [
    {
      "id": "sync",
      "label": "Sync data",
      "type": "command",
      "command": "scripts/sync.sh"
    }
  ]
}
```

#### API Action

```json
{
  "actions": [
    {
      "id": "create_release",
      "label": "Create GitHub release",
      "type": "api",
      "method": "POST",
      "endpoint": "/repos/{owner}/{repo}/releases",
      "payload": {
        "tag_name": "{{release.tag}}",
        "name": "{{release.name}}",
        "body": "{{release.notes}}"
      }
    }
  ]
}
```

## Release Actions Configuration

Release actions are normal extension `actions` whose IDs start with
`release.`. Core release planning detects configured extensions with release
actions and routes prepare/package/publish behavior through those actions.

```json
{
  "actions": [
    {
      "id": "release.publish.github",
      "label": "Publish GitHub release",
      "type": "command",
      "command": "scripts/publish-github-release.sh"
    }
  ]
}
```

### Release Action Types

- **`command`**: Execute an extension-owned command.
- **`api`**: Execute an API request configured by the extension action.
- **`builtin`**: Legacy UI action type parsed by the CLI but not executed.

### Release Action Output Contract

Release actions should return JSON that is generic to the action outcome, not to a language or package manager. Core release rendering understands these status values:

- **`success: true`**: The action completed successfully.
- **`status: "skipped"`** with `success: false`: The action intentionally did nothing.
- **`status: "missing_secret"`** with `success: false`: A required token or credential is not configured.
- **`status: "auth_required"`** with `success: false`: The user must authenticate before the action can run.

For skipped or authentication-related results, include **`reason`** or **`message`** with the human-readable explanation. Core surfaces that explanation in the release step warning without parsing ecosystem-specific command output.

```json
{
  "success": false,
  "status": "missing_secret",
  "reason": "Registry token is not configured"
}
```

#### Example

```json
{
  "actions": [
    {
      "id": "release.publish.github",
      "label": "Publish GitHub release",
      "type": "command",
      "command": "scripts/publish-github-release.sh"
    }
  ]
}
```

## Hooks Configuration

Extensions can declare lifecycle hooks that run at named events. Extension hooks execute before component hooks, providing platform-level behavior.

```json
{
  "hooks": {
    "post:version:bump": ["package-manager refresh-lockfile"],
    "post:deploy": [
      "wp cache flush --path={{base_path}} --allow-root 2>/dev/null || true"
    ]
  }
}
```

### Hooks Fields

- **`hooks`** (object): Map of event names to command arrays
  - Keys: event name (e.g., `pre:version:bump`, `post:version:bump`, `post:release`, `post:deploy`)
  - Values: array of shell command strings

Use `post:version:bump` for generated artifacts that must reflect the new version before the release commit, such as lockfiles, generated manifests, or version-derived build metadata.

Most hooks execute locally in the component's directory. `post:deploy` hooks execute **remotely via SSH** with template variable expansion:

| Variable | Description |
|----------|-------------|
| `{{component_id}}` | The component ID |
| `{{install_dir}}` | Remote install directory (base_path + remote_path) |
| `{{base_path}}` | Project base path on the remote server |

See [hooks architecture](../architecture/hooks.md) for details on execution order and failure modes.

## Documentation Configuration

Extensions can provide embedded documentation.

```json
{
  "docs": [
    "overview.md",
    "commands/wp-cli.md"
  ]
}
```

Documentation files live in the extension's `docs/` directory. Topics resolve to `homeboy docs <extension_id>/<topic>`.

## Capabilities and Storage Backend

```json
{
  "capabilities": ["storage"],
  "storage_backend": "filesystem"
}
```

- **`capabilities`**: Array of capability strings (e.g., `["storage"]`)
- **`storage_backend`**: Storage backend identifier when providing storage capability

## Complete Example

```json
{
  "name": "WordPress",
  "id": "wordpress",
  "version": "1.0.0",
  "description": "WordPress platform integration with WP-CLI",
  "runtime": {
    "runtimes": {
      "php": { "version": "8.2" }
    }
  },
  "executable": {
    "runtime": {
      "run_command": "wp {{args}}",
      "setup_command": "curl -O https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar && chmod +x wp-cli.phar && sudo mv wp-cli.phar /usr/local/bin/wp",
      "ready_check": "wp --version"
    }
  },
  "platform": {
    "database": {
      "cli": {
        "connect": "wp db cli",
        "query": "wp db query \"{{query}}\"",
        "tables": "wp db tables",
        "describe": "wp db describe {{table}}"
      },
      "defaults": {
        "host": "localhost",
        "port": 3306,
        "user": "root"
      }
    },
    "version_patterns": [
      {
        "file": "style.css",
        "pattern": "Version:\\s*([\\d.]+)"
      }
    ]
  },
  "commands": {
    "wp": {
      "description": "Run WP-CLI commands",
      "run_command": "wp {{args}}",
      "help": "Execute WP-CLI commands in the project context"
    }
  },
  "docs": [
    "overview.md",
    "commands/wp-cli.md"
  ]
}
```

## Storage Location

Extension manifests are stored in the extension directory:
- Git extensions: `~/.config/homeboy/extensions/<extension_id>/<extension_id>.json`
- Symlinked extensions: `<source_path>/<extension_id>.json`

## Related

- [Extension command](../commands/extension.md) - Manage extension installation and execution
- [Template variables](../templates.md) - Variable reference for templates
