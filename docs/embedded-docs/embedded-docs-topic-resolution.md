# Embedded docs: topic resolution and keys

Homeboy embeds markdown files from the repository’s top-level `docs/` directory into the binary at build time.

## Key mapping (topic → embedded key)

Embedded documentation keys are derived from markdown file paths:

- Root: repository `docs/`
- Key: relative path from `docs/`, with OS separators normalized to `/`
- Key: `.md` extension removed

Examples:

- `docs/index.md` → key `index`
- `docs/changelog.md` → key `changelog`
- `docs/commands/docs.md` → key `commands/docs`

## `homeboy docs <topic...>` normalization

`homeboy docs` accepts a topic as trailing arguments. Resolution:

- No topic args → `(topic_label="index", key="index")`
- Each arg is split on `/` and each segment is normalized.
- Empty segments are removed.
- Key is `segments.join("/")`.
- `topic_label` is the user input joined with spaces (e.g. `"project set"`).

If normalization yields no segments, the key falls back to `index` and `topic_label` becomes `unknown`.

Segment normalization is performed by `homeboy_core::token::normalize_doc_segment`.

## Available topics list format

`available_topics` is returned as a single comma+space separated string:

```text
index, changelog, commands/docs, ...
```

Topics are sorted lexicographically.

## Related

- [Docs command](../commands/docs.md)
- [Changelog command](../commands/changelog.md)
