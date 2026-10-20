# `homeboy deps`

Manage component dependencies.

## Synopsis

```sh
homeboy deps <COMMAND>
```

## Subcommands

- `status` — inspect dependency constraints and locked package versions
- `install` — install dependencies through detected providers
- `update` — update one package through its dependency provider
- `stack status` — list declared dependency stack edges
- `stack plan <upstream>` — plan downstream updates for a merged upstream component or repo
- `stack apply <upstream> [--to <constraint>] [--dry-run]` — run declared update, post-update, and test commands in dependency order
- `stack outdated [component]` — report declared edges against each source's newest release
- `stack refresh [component] [--path <path>] [--dry-run] [--no-install]` — roll declared edges forward to each source's newest release

## Dependency Stacks

Components can declare deterministic downstream propagation edges in `homeboy.json`:

```json
{
  "dependency_stack": [
    {
      "upstream": "example-org/html-to-blocks-converter",
      "downstream": "block-format-bridge",
      "package": "example-org/html-to-blocks-converter",
      "post_update": ["composer build"],
      "test": ["homeboy review test --path . --extension wordpress"]
    },
    {
      "upstream": "block-format-bridge",
      "downstream": "static-site-importer",
      "package": "example-org/block-format-bridge",
      "update": "composer update example-org/block-format-bridge --with-dependencies --no-interaction",
      "test": ["homeboy review test --path . --extension wordpress"]
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

### Checking declared edges against released versions (`stack outdated`)

An edge can declare a remote release `source` so the downstream resolves its
upstream's newest release without the upstream being registered locally:

```json
{
  "dependency_stack": [
    {
      "upstream": "example-org/html-to-blocks-converter",
      "downstream": "block-format-bridge",
      "package": "example-org/html-to-blocks-converter",
      "source": {
        "repo": "git@github.com:example-org/components.git",
        "prefix": "html-to-blocks-converter"
      }
    }
  ]
}
```

`homeboy deps stack outdated` runs from the downstream checkout and reports
each edge where this component is the `downstream` as `current`, `behind`, or
`unresolvable`. An edge that cannot be checked never reads as current. The
exit status gates on the result: `0` when every edge is current, `1` when at
least one edge is behind, `2` when at least one edge is unresolvable and none
are behind.

### Rolling edges forward (`stack refresh`)

`homeboy deps stack refresh` is the verb a scheduled CI job calls from the
downstream checkout (or with `--path`). It runs the same evaluation as
`stack outdated`, then applies each `behind` edge through the edge's existing
update path — the same step `stack apply` runs: the edge's `update` command if
set, otherwise the provider-backed default update with `--to <newest>`, then
install/lockfile refresh unless `--no-install`, then `post_update`. It does
not commit, push, or open PRs; landing the change belongs to the release plan
and to CI automation.

Each edge reports `from`, `to`, a `state` (`updated`, `current`,
`unresolvable`, `failed`), and a `breaking` verdict (`true`, `false`, or
`null`) with `breaking_reasons`. A range is breaking when:

- the major version changes;
- the major is 0 and the minor changes (0.x minors carry behavior changes);
- a commit in the release range carries a conventional breaking marker
  (`type!:` / `type(scope)!:` subject, or a `BREAKING CHANGE` /
  `BREAKING-CHANGE` footer), or the target release tag's notes do.

When the range cannot be inspected (for example the locked version was never
released under the source's tag namespace), the verdict is `null` with a
reason, and callers must treat `null` as breaking. The JSON output carries a
top-level `breaking` that is true when any updated edge is breaking or `null`.

`--dry-run` prints the plan (including the breaking verdicts, which are
read-only) and changes nothing. Exit status: `0` when nothing is behind or
everything behind updated cleanly, `1` when at least one edge failed to
update, `2` when at least one edge was unresolvable and none failed.

## Provider Boundary

`homeboy deps` is provider-backed. Core resolves dependency providers from the
component/workspace, then calls a generic adapter contract for status, package
matching, updates, and installs. Composer, npm/pnpm, component scripts, and
extension-backed providers are implementation details behind that boundary.

### Declarative dependency manifest (`deps` in `homeboy.json`)

A repo declares how its dependencies install, update, and report status under
a top-level `deps` object in its `homeboy.json` — the one repo-level config
file. There is no side-file manifest; a checkout still carrying the retired
side file gets a hard error pointing at `homeboy.json`.

```json
{
  "id": "my-component",
  "deps": {
    "provider": "composer",
    "dependency_identities": ["example-org/my-component"],
    "packages": [
      {
        "name": "example-org/library",
        "manifest_section": "require",
        "constraint": "^1.0",
        "locked_version": "1.2.3"
      }
    ],
    "commands": {
      "install": { "argv": ["composer", "install", "--no-interaction"] },
      "update": { "argv": ["composer", "update", "{package}", "{constraint}"] },
      "reusable": { "argv": ["composer", "check-platform-reqs"] }
    },
    "outputs": [{ "path": "vendor", "kind": "directory" }]
  }
}
```

- `provider` (required) labels the package manager in results.
- `commands.install`/`update`/`reusable` declare argv-form commands with an
  optional `cwd` (relative to the workspace) and `success_exit_codes`.
  `{package}` and `{constraint}` expand in `update` argv entries.
- `outputs` list workspace-relative paths (`path`, `file`, or `directory`
  kinds) that a successful install must materialize.
- `packages` and `dependency_identities` make status and stack resolution
  declarative.

Provider resolution order: the `deps` declaration in `homeboy.json` (either
the declarative manifest above or a full dependency adapter manifest carrying
the `homeboy-extension/dependency-adapter-manifest/v1` schema), then installed
dependency adapters, then the component's `scripts.deps`, then extension deps
providers.

Callers should use `homeboy deps status`, `homeboy deps install`, `homeboy deps
update`, or `homeboy component setup` instead of scripting package-manager
literals directly. New ecosystems should add or move provider adapters without
changing dependency command orchestration.

### Hydration readiness

An adapter's upward selection binds installation and reusable-state checks to
the nearest directory containing its declared selection signal. Package
identity and package updates retain the requested project scope. Install outputs
are relative to the install command's directory, consistently on the controller
and in the portable Lab plan.

Hydration reuses dependencies only when the provider's reusable-state check
passes and all declared outputs are ready. Otherwise it runs the declared
installer and validates its outputs before reporting success. Directory outputs
must contain an entry; an empty dependency directory is not installed state.
Failed output validation records the provider, command, install directory, and
exact `missing_outputs` paths in the hydration outcome.

## Related

- [component](component.md)
