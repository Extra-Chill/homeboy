# Embedded docs: topic resolution and keys

Homeboy embeds markdown files from `homeboy-cli/docs/` into the CLI binary at build time.

In addition, `homeboy docs` reads documentation provided by installed modules from:

- `<config dir>/homeboy/modules/<moduleId>/docs/`

Module docs use the same key format as embedded docs (relative path within the module's `docs/` directory, without `.md`).

## Key mapping (topic → embedded key)

Embedded documentation keys are derived from markdown file paths:

- Root: `homeboy-cli/docs/`
- Key: relative path from `homeboy-cli/docs/`, with OS separators normalized to `/`
- Key: `.md` extension removed

Examples:

- `homeboy-cli/docs/index.md` → key `index`
- `homeboy-cli/docs/changelog.md` → key `changelog`
- `homeboy-cli/docs/commands/docs.md` → key `commands/docs`

## `homeboy docs <topic...>` normalization

`homeboy docs` accepts a topic as trailing arguments. Resolution:

- No topic args → `(topic_label="index", key="index")`
- Each arg is split on `/` and each segment is normalized.
- Empty segments are removed.
- Key is `segments.join("/")`.
- `topic_label` is the user input joined with spaces (e.g. `"project set"`).

If normalization yields no segments (for example: topic args are only whitespace or only `/`), the command behaves as if no topic was provided (defaults to `index`). In this case `topic_label` is set to `"unknown"` (the resolved key still becomes `index`).

If the resolved key does not exist in embedded core docs or module docs, `homeboy docs` returns an error.

Segment normalization is performed by `homeboy::token::normalize_doc_segment`.

## Available topics list format

`available_topics` is returned as a JSON array of embedded keys:

```json
["changelog", "commands/build", "commands/docs", "index"]
```

Topics are sorted lexicographically.

## Related

- [Docs command](../commands/docs.md)
- [Changelog command](../commands/changelog.md)
