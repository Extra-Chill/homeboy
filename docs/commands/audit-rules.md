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
