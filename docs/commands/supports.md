# supports

Check whether a command supports a given option/flag. This is intended for wrappers and CI tooling
that need deterministic fallback behavior without scraping `--help` output.

## Usage

```bash
homeboy supports <command> <option>
```

### Examples

```bash
homeboy supports test --changed-since
homeboy supports "docs audit" --path
homeboy supports audit --json-summary
```

## Output

Returns JSON with:

- `command` — normalized command path
- `option` — queried option
- `supported` — boolean
- `known_options` — known options for the command (if command exists)
- `hint` — guidance for unknown command/option

Exit code:

- `0` when supported
- `1` when unsupported/unknown
