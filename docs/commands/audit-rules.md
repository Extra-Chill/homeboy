# Audit Layer Ownership Rules

Homeboy audit supports optional architecture/layer ownership rules to catch design-level drift that
passes lint/static checks.

## Rule file locations

Use one of:

- `homeboy.json` under `audit_rules`

## Example

```json
{
  "layer_rules": [
    {
      "name": "engine-owns-terminal-status",
      "forbid": {
        "glob": "inc/Core/Steps/**/*.php",
        "patterns": ["JobStatus::", "datamachine_fail_job"]
      },
      "allow": {
        "glob": "inc/Abilities/Engine/**/*.php"
      }
    }
  ]
}
```

## Finding output

- `convention`: `layer_ownership`
- `kind`: `layer_ownership_violation`
- severity: warning

These findings participate in baseline comparisons like any other audit finding.

## Config Key Usage Rules

`audit.config_key_usage.rules` lets a component provide language/framework-specific regexes for config keys that are written, migrated, or exposed by accessors. Homeboy core only correlates configured captures across fingerprints; it does not know what a given key means.

Example:

```json
{
  "audit": {
    "config_key_usage": {
      "rules": [
        {
          "id": "workflow-config",
          "exclude_path_contains": ["fixtures/", "vendor/"],
          "write_patterns": [
            {
              "pattern": "set_config\\(\\s*['\"](?P<key>[a-z_]+)['\"]"
            }
          ],
          "accessor_patterns": [
            {
              "pattern": "function\\s+(?P<symbol>[a-z_]+)\\(.*get_config\\(\\s*['\"](?P<key>[a-z_]+)['\"]",
              "symbol_capture": "symbol"
            }
          ],
          "read_patterns": [
            {
              "pattern": "read_config\\(\\s*['\"](?P<key>[a-z_]+)['\"]"
            }
          ],
          "accessor_symbol_read_patterns": [
            "\\b{symbol}\\s*\\("
          ]
        }
      ]
    }
  }
}
```

Finding output:

- `convention`: `config_key_usage:<rule id>`
- `kind`: `write_only_config_key`
- severity: warning

Reads in test paths do not satisfy the rule. If an accessor pattern captures `symbol_capture`, Homeboy can treat non-test references to that symbol outside the accessor definition file as production reads. By default, symbols must appear as full identifier tokens on non-comment lines; `accessor_symbol_read_patterns` can provide component-owned regex templates when a language or framework needs stricter call syntax. `{symbol}` is replaced with the escaped accessor symbol.

## Mutating Resource Access

`audit.mutating_resource_access` is a generic marker-driven detector for handler
paths that mutate resource identifiers without a configured ownership/access
check. Homeboy core does not know any framework names; components or extensions
provide the registration markers, mutating operation markers, resource-id regexes,
accepted access helpers, trusted delegation markers, and mutator markers.

```json
{
  "audit": {
    "mutating_resource_access": {
      "handler_registration_markers": ["route("],
      "mutating_operation_markers": ["POST", "PATCH", "DELETE", "EDITABLE"],
      "resource_identifier_patterns": ["\\b(flow_id|pipeline_id|agent_id|post_id)\\b"],
      "access_helper_markers": ["PermissionHelper::owns_agent_resource", "can_access_agent"],
      "trusted_delegation_markers": ["CheckedAbility"],
      "mutator_markers": ["update_", "delete_", "save_"]
    }
  }
}
```

Finding output:

- `convention`: `mutating_resource_access`
- `kind`: `mutating_resource_access`
- severity: warning

Only configured `access_helper_markers` and `trusted_delegation_markers` suppress
findings. Core treats both lists as opaque markers and does not infer access
helpers from function names.

# Requested Detector Rules

`audit_rules.requested_detectors` lets an extension provide generic text detectors
without baking ecosystem terms into Homeboy core. Core owns the matching primitive;
the component supplies the sink, scope, and allowlist markers.

# Source Policy Rules

`audit.source_policies` lets a component or extension define generic source
boundary rules without baking domain terms into Homeboy core. The first source
policy primitive is `type: "forbidden_terms"`, which scans shared audit
fingerprints for configured token, literal, or regex terms inside configured path
scopes.

Use source policies for architecture boundaries such as core-layer purity,
detector implementation neutrality, or product/domain terms that belong in
component-owned config rather than generic core code.

Adapter/service layer boundaries are a natural use for this generic surface:
a component can configure adapter paths and the responsibility markers that
belong in its service layer. Homeboy's own `homeboy.json` uses this primitive
for the `thin-command-adapters` rule. The configured Homeboy boundary is:

```text
src/commands/* = clap args + typed request construction + output adaptation
src/core/* = domain policy, orchestration, persistence, execution, artifacts
```

That Homeboy-owned config scans command modules for direct process execution,
filesystem mutation, run-artifact persistence, and runner orchestration markers.
Existing orchestration-heavy command modules are allowlisted as transitional
extraction targets; new command modules should delegate those responsibilities
to `core` services instead of adding local exceptions. Other repositories can
define their own adapter paths, service-layer markers, and transitional
allowlists without changing Homeboy audit code.

Example:

```json
{
  "audit": {
    "source_policies": [
      {
        "id": "core-layer-boundary",
        "kind": "source_policy_violation",
        "severity": "warning",
        "convention": "source_policy",
        "language": "rust",
        "file_extensions": ["rs"],
        "include_path_contains": ["src/core/"],
        "exclude_path_contains": ["src/core/fixtures/allowed"],
        "allow_line_contains": ["homeboy-audit: allow-source-policy"],
        "ignore_line_prefixes": ["//", "///", "//!"],
        "ignore_after_line_equals": ["#[cfg(test)]"],
        "example_path_contains": ["/fixtures/", "/examples/"],
        "type": "forbidden_terms",
        "terms": [
          {
            "value": "crate::commands::",
            "label": "command-layer dependency",
            "match_mode": "literal"
          }
        ],
        "default_match": "literal",
        "case_insensitive": false,
        "description": "Source policy term `{term}` appears at line {line} in {classification} context `{context}`",
        "suggestion": "Move `{term}` behind an injected adapter owned outside the scanned source scope."
      }
    ]
  }
}
```

Supported match modes are `token`, `literal`, and `regex`. Templates support
`{term}`, `{line}`, `{classification}`, and `{context}`. Existing
`audit.core_boundary_leaks` behavior remains available and is implemented as a
compatibility wrapper over this source-policy primitive.

### Scoped Proxy Drift

Use `type: "scoped_proxy"` when docs/schema describe a helper as scoped to an
internal namespace but the implementation forwards request-controlled paths to a
local proxy sink. The detector flags files that match all of:

- `claim_pattern`: docs/schema text that claims an internal namespace or scoped API
- `target_pattern`: request input used as the forwarded target/path
- `sink_pattern`: the local forwarding call
- no `allowlist_pattern`: prefix/allowlist validation for the documented scope

Example:

```json
{
  "requested_detectors": [
    {
      "id": "internal-proxy-scope",
      "kind": "proxy_scope_drift",
      "severity": "warning",
      "language": "php",
      "file_extensions": ["php"],
      "type": "scoped_proxy",
      "claim_pattern": "(?i)\\b(internal API|/acme/v1)\\b",
      "target_pattern": "\\$input\\s*\\[\\s*['\"]path['\"]\\s*\\]",
      "sink_pattern": "\\b(?P<sink>forward_internal_request)\\s*\\(",
      "allowlist_pattern": "(?i)(str_starts_with|preg_match)\\s*\\([^;]*(/acme/v1|allowed_prefixes|allowlist)",
      "description": "Proxy scope drift at line {line}: scoped docs feed `{sink}` from request input without a matching allowlist",
      "suggestion": "Add an allowlist/prefix check for the documented scope or document the proxy as general-purpose."
    }
  ]
  }
}
```

Finding output:

- `convention`: defaults to `requested_detectors`
- `kind`: `proxy_scope_drift`
- severity: configured by the rule, usually warning

## Public registry exposure

`audit.public_registry_exposure` detects public endpoint windows that return raw
registry/config/status metadata getters while a permission-aware resolver or
helper exists in an explicitly configured resolver scope. The rule is generic:
projects provide route markers, public-access markers, raw getter regexes,
resolver regexes, route-window sizing, resolver proximity settings, and
allowlists. Homeboy only correlates those configured signals.

```json
{
  "audit": {
    "public_registry_exposure": {
      "route_markers": ["register_route("],
      "public_access_markers": ["allow_public"],
      "raw_getter_patterns": ["get_[A-Za-z_]*registry\\(\\)"],
      "permission_aware_resolver_patterns": ["PermissionAware[A-Za-z_]*Resolver"],
      "route_context_lines": 8,
      "resolver_path_contains": ["src/policy/", "src/resolvers/"],
      "resolver_same_namespace": true,
      "allow_path_contains": ["public-discovery"],
      "allow_line_contains": ["homeboy-audit: allow-public-registry-exposure"]
    }
  }
}
```

Finding output:

- `convention`: `public_registry_exposure`
- `kind`: `public_registry_exposure`
- severity: warning

## Required Regex

Use `type: "required_regex"` when a risky candidate match must have a companion
match in a defined scope.

```json
{
  "audit_rules": {
    "requested_detectors": [
      {
        "id": "redirect-dominance",
        "kind": "undominated_redirect_param",
        "language": "php",
        "file_extensions": ["php"],
        "type": "required_regex",
        "pattern": "wp_redirect\\s*\\(\\s*\\$(?P<var>[A-Za-z_][A-Za-z0-9_]*)",
        "required_pattern": "validate_[A-Za-z0-9_]+\\s*\\([^;]*\\${var}",
        "required_scope": "before_match",
        "description": "Redirect at line {line} uses `${var}` before validation dominates it",
        "suggestion": "Validate `${var}` before every redirect branch."
      }
    ]
  }
}
```

Supported scopes are `same_file`, `before_match`, `after_match`, and
`any_eligible_file`.

## Derived Absence

Use `type: "derived_absence"` when values collected from one shape must appear in
a second shape elsewhere in the eligible corpus. This is useful for write-only
config keys and import/export schema drift checks.

```json
{
  "audit_rules": {
    "requested_detectors": [
      {
        "id": "config-write-only",
        "kind": "config_key_write_only",
        "language": "php",
        "file_extensions": ["php"],
        "type": "derived_absence",
        "source_pattern": "\\$config\\s*\\[\\s*['\"](?P<key>[A-Za-z_][A-Za-z0-9_]*)['\"]\\s*\\]\\s*=",
        "value_capture": "key",
        "label": "config key `{key}`",
        "required_pattern": "\\$config\\s*\\[\\s*['\"]{value}['\"]\\s*\\]",
        "exclude_required_path_contains": ["tests/", "fixtures/"],
        "description": "{label} written at line {line} has no non-test consumer",
        "suggestion": "Consume `{value}` in production code or remove the stale config write."
      }
    ]
  }
}
```

Templates support regex capture names and `{line}`. `derived_absence` also exposes
`{value}` and `{label}`.

## Redirect Validation Dominance

`audit.redirect_validation` configures a warning-level security-adjacent detector for request-derived redirect destinations. Core stays framework-agnostic: components or extensions provide the request parameter markers, request source markers or regex patterns, redirect sink markers, and validation/allowlist markers for their runtime.

Example:

```json
{
  "audit": {
    "redirect_validation": {
      "request_names": ["'redirect_uri'", "'return_to'", "'callback_url'"],
      "request_source_markers": ["query.", "body.", "$_GET[", "$_POST["],
      "request_source_patterns": ["\\brequest\\.(query|body)\\."],
      "redirect_sinks": ["redirect_to(", "Location:"],
      "validation_markers": ["allow_redirect_destination", "validate_redirect_destination"],
      "file_extensions": ["php"]
    }
  }
}
```

The detector tracks variables assigned from configured request-name markers on lines that also include configured request source markers or patterns, then reports `redirect_validation` when a configured redirect sink uses the variable before a configured validation marker dominates that sink by line order and block depth. This is a heuristic line/block-depth check, not CFG evidence, so findings require reviewer judgment.

## Requested Config Round-Trip Key Detector

Extensions and components can configure a generic requested detector that compares
config-object key sets across export/import/copy allowlists and behavior-bearing
read/write sites. Homeboy core only evaluates the configured regexes and compares
captured key strings; framework-specific semantics and intentional runtime-only key
exclusions belong in the extension or component config.

```json
{
  "audit_rules": {
    "requested_detectors": [
      {
        "id": "flow-step-config-roundtrip",
        "kind": "config_roundtrip_asymmetry",
        "severity": "warning",
        "convention": "requested_detectors",
        "language": "php",
        "file_extensions": ["php"],
        "type": "config_roundtrip_keys",
        "object": "flow step config",
        "export_pattern": "'(?P<key>[a-z_]+)'\\s*=>\\s*\\$config\\[",
        "import_pattern": "\\$config\\['(?P<key>[a-z_]+)'\\]\\s*=",
        "copy_patterns": ["\\$copy\\['(?P<key>[a-z_]+)'\\]\\s*="],
        "behavior_pattern": "\\$config\\['(?P<key>[a-z_]+)'\\]",
        "exclude_key_patterns": ["^runtime_"],
        "description": "{object} key `{key}` is missing from {missing} round-trip side(s)",
        "suggestion": "Review `{key}` and add it to the missing allowlist, or exclude it as runtime-only."
      }
    ]
  }
}
```

The detector emits `config_roundtrip_asymmetry` when a behavior-bearing key is
absent from export or import, or when export/import key allowlists disagree for a
key not excluded by `exclude_key_patterns`. When `copy_patterns` is configured,
copy allowlists participate in the same comparison. Template variables include
`object`, `key`, `missing`, `line`, `export_count`, `import_count`, `copy_count`,
and `behavior_count`.
