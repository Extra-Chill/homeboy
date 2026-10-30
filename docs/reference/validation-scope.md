# Lint and test scope exclusions

Lint and test (including `review lint` and `review test`) merge
`scopes.defaults.exclude` with `scopes.lint.exclude` or `scopes.test.exclude`
from the component's `homeboy.json`. Excludes are additive and deduplicated.

Homeboy also excludes nested component directories containing their own
`homeboy.json`, even without explicit scope configuration. Discovery uses the
execution checkout (including path overrides/worktrees), stops at nested
component boundaries, and does not follow directory symlinks. `.git`, `vendor`,
`node_modules`, and `target` are not searched for nested components.
Deploy/release scope behavior is unchanged.

## Extension env contract

`HOMEBOY_SCOPE_EXCLUDE` is always supplied to lint/test extensions. Its value is
newline-separated component-root-relative paths or glob patterns, with no
trailing newline. An empty value means no exclusions. Paths are relative to the
component being validated, not the extension installation or a containing Git
repository. Automatically discovered directories have a trailing `/`.

Extensions should translate these excludes into each tool's ignore mechanism
(phpcs, phpstan, eslint, test runners, etc.) before execution. Plain paths exclude
the named file/directory and all descendants; glob patterns use glob-match
semantics. Excludes apply even when explicit files or changed-file selections
are requested. The variable is independent of `HOMEBOY_EXCLUDE_SNIFFS` and
`HOMEBOY_TEST_SCOPE_*` (changed-test routing).

Core also removes collected lint/test findings whose file locations fall under
these exclusions. Relative locations and absolute locations beneath the component
root are supported. Locationless findings are retained. Raw sidecars remain
unaltered as execution evidence. Filtering does not suppress infrastructure
errors or rewrite test execution results; extensions must honor the contract to
avoid executing excluded code. WordPress runner support is a separate
`homeboy-extensions` change.
