# `homeboy projects`

## Synopsis

```sh
homeboy projects [--current]
```

## Flags

- `--current`: return only the active project ID.

## JSON output

### `homeboy projects` (list)

```json
{
  "command": "projects.list",
  "activeProjectId": "<id>|null",
  "projects": [
    {
      "id": "<id>",
      "name": "<name>",
      "domain": "<domain>",
      "projectType": "<type>",
      "active": true
    }
  ]
}
```

### `homeboy projects --current`

```json
{
  "command": "projects.current",
  "activeProjectId": "<id>|null",
  "projects": null
}
```

## Related

- [project](project.md)
- [JSON output contract](../json-output/json-output-contract.md)
