# `homeboy deps`

Manage component dependencies.

## Synopsis

```sh
homeboy deps <COMMAND>
```

## Subcommands

- `status` — inspect dependency constraints and locked package versions
- `update` — update one package through its dependency provider
- `stack status` — list declared dependency stack edges
- `stack plan <upstream>` — plan downstream updates for a merged upstream component or repo
- `stack apply <upstream> [--to <constraint>] [--dry-run]` — run declared update, post-update, and test commands in dependency order

## Dependency Stacks

Components can declare deterministic downstream propagation edges in `homeboy.json`:

```json
{
  "dependency_stack": [
    {
      "upstream": "chubes4/html-to-blocks-converter",
      "downstream": "block-format-bridge",
      "package": "chubes4/html-to-blocks-converter",
      "post_update": ["composer build"],
      "test": ["homeboy test --path . --extension wordpress"]
    },
    {
      "upstream": "block-format-bridge",
      "downstream": "static-site-importer",
      "package": "chubes4/block-format-bridge",
      "update": "composer update chubes4/block-format-bridge --with-dependencies --no-interaction",
      "test": ["homeboy test --path . --extension wordpress"]
    }
  ]
}
```

When `update` is omitted, Homeboy uses:

```sh
homeboy deps update <package> --path <downstream path>
```

Pass `--to <constraint>` to `stack apply` when the default provider-backed update should set a new manifest constraint:

```sh
homeboy deps stack apply extrachill-components --to ^0.5.2
```

Custom `dependency_stack[].update` commands are executed as declared; include any constraint handling directly in the custom command when overriding the default provider-backed update.

## Related

- [component](component.md)
