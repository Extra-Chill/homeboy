# version

Manage component versions.

```bash
homeboy version show <component-id>
homeboy version bump <component-id> <bump-type>
```

## Subcommands

### show

Show current version of a component.

```bash
homeboy version show <component-id>
```

### bump

Increment the version of a component.

```bash
homeboy version bump <component-id> <bump-type>
```

**Arguments**:
- `component-id` - The ID of the component to bump
- `bump-type` - One of: `patch`, `minor`, `major`

## Requirements

- Component must have `versionFile` configured in its configuration.
- The version file must contain a version string matching the `versionPattern` (or a default pattern if not specified).
