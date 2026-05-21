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

Use `access_helper_markers` for direct ownership/access checks in the handler.
Use `trusted_delegation_markers` for helper or ability paths that the component
has verified enforce equivalent ownership/access checks.
