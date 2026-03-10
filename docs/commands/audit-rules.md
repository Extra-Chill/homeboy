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
