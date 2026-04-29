# `homeboy version`

## Synopsis

```sh
homeboy version show [<component_id>] [--path <path>]
```

## Description

`homeboy version show` is read-only version inspection. It reports the current version for a component discovered from the current directory, an explicit component ID, or an explicit `--path`. If no component can be discovered, it reports the Homeboy binary version.

Changing versions and cutting releases belongs to [`homeboy release`](release.md):

```sh
homeboy release <component_id> --bump patch|minor|major|x.y.z
```

## Arguments

- `[<component_id>]`: component ID to inspect.

## Options

- `--path <path>`: override the source root for component version lookup.

## JSON Output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). `homeboy version show` returns a `VersionOutput` object as the `data` payload.

Payload fields:

- `command`: `version.show`
- `component_id`: discovered or explicit component ID, omitted for the Homeboy binary version
- `version`: detected current version
- `targets`: array of `{ file, pattern, full_path, match_count }`

## Exit Code

- `0` on success.
- Non-zero if the component version cannot be parsed.

## Related

- [release](release.md)
- [changes](changes.md)
- [component](component.md)
