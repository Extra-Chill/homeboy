# config

Manage the global Homeboy config file (`config.json`).

- Path: `dirs::data_dir()/Homeboy/config.json` (see `homeboy config path`)
- This does not edit project/component config files.

## Commands

### `homeboy config path`

Print the resolved path to the global `config.json`.

### `homeboy config show`

Show the current global config. If the file does not exist yet, Homeboy returns the default struct (mostly empty/`null` values).

### `homeboy config keys`

List the known keys that `homeboy config set`/`unset` supports.

### `homeboy config set <key> <value>`

Set a known key.

- String keys: pass the string directly
- String array keys: pass a comma-separated list (whitespace is trimmed)

Examples:

- `homeboy config set activeProjectId extrachill`
- `homeboy config set defaultChangelogNextSectionLabel Unreleased`
- `homeboy config set defaultChangelogNextSectionAliases "Unreleased,Next"`

### `homeboy config unset <key>`

Unset (remove) a known key.

Example:

- `homeboy config unset defaultChangelogNextSectionAliases`

### `homeboy config set-json <pointer> <json> [--allow-unknown]`

Escape hatch for setting a raw JSON value at a JSON pointer.

- If `<pointer>` is not in the known-key registry, you must pass `--allow-unknown`.
- `<json>` must be valid JSON (e.g. `"hello"`, `123`, `true`, `[]`, `{}`).

Examples:

- `homeboy config set-json /defaultChangelogNextSectionAliases '["Unreleased","Next"]'`
- `homeboy config set-json /someNewKey '{"a":1}' --allow-unknown`
