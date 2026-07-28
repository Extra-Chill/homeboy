# `homeboy harvest`

```text
homeboy harvest <project-id|component-id> [component-id...] [--check|--dry-run|--apply] [--exclude <glob>]... [--author <name-and-email>]
```

`harvest` compares the bytes under every selected component's configured `remote_path` with its `local_path`, using the project's configured SSH transport. It is bounded to managed components and has no product-specific path rules.

- `--check` reports additions, modifications, and deletions without writing local files. Drift exits with code 2.
- `--dry-run` produces the same non-mutating content-change plan for review.
- `--apply` materializes one component's remote changes, including remote deletions, and commits them. It refuses a dirty local Git worktree rather than choosing between local and remote content. Apply reviewed components separately so each recovery produces one provenance-bearing commit.
- `--exclude` accepts repeatable relative globs. Existing component ignore and source-snapshot exclusion policy is also honored. Version-control metadata and Homeboy's own `.homeboy-*` transport scratch files are always excluded, on both sides.
- `--author` sets the Git author for the recovery commit. The normal local Git identity remains the committer. The commit message records `Harvested-from: <project>:<remote-path>` provenance.

Binary files are compared and copied as bytes. Remote-only files are additions; local-only files are deletions.

```sh
homeboy harvest production --check
homeboy harvest production api --dry-run
homeboy harvest production api --apply --author 'Remote agent <agent@example.invalid>'
```

## Deploy drift protection

Deploy already protects remote drift through the in-place content manifest added for [issue #10058](https://github.com/Extra-Chill/homeboy/issues/10058). Harvest uses the generic `content_diff` primitive after materializing the bounded remote component because recovery requires the actual bytes, while deploy checks need only compact remote hash evidence.

Both answers are produced by the same directory walk (`content_diff::scan_tree`), so they can no longer disagree about what is in a tree ([issue #10290](https://github.com/Extra-Chill/homeboy/issues/10290)). The two call sites still differ, but only through named options:

| | `harvest` | `deploy --check` |
|---|---|---|
| question | "what did the target change that my checkout does not have?" | "does the installed tree still hold the bytes the package contract says it should?" |
| excludes | component ignore + source-snapshot policy (including `.gitignore`-derived entries) + `--exclude` | none |
| symlinks | not recorded | recorded, compared by link target |
| file mode | not recorded | executable bit only |

`deploy --check` deliberately does **not** inherit the `.gitignore`-derived excludes. A deployed package routinely ships directories a source checkout ignores (vendored dependencies, built assets); silencing those would turn a false positive into a false negative in a drift detector.
