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

Reads in test paths do not satisfy the rule. If an accessor pattern captures `symbol_capture`, a non-test reference to that symbol outside the accessor definition file counts as a production read.
