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

## Requested Detectors

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
```

Finding output:

- `convention`: defaults to `requested_detectors`
- `kind`: `proxy_scope_drift`
- severity: configured by the rule, usually warning
