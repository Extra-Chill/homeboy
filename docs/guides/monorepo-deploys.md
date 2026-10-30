# Deploy only changed monorepo components

Extra Chill's `extrachill-network` uses the wpcom model: merging to `main`
deploys that commit, with no per-component release or version bump. Components
live at `themes/extrachill`, `plugins/extrachill-*`, `packages/*`, and the repo
root. Each has its own `homeboy.json` with a unique `id`.

```sh
homeboy component changed --base "$BEFORE" --head "$SHA" --path . \
  --shared-path '.github/**' --shared-path 'tooling/**' --format matrix
# {"include":[{"component":"extrachill-api","path":"plugins/extrachill-api"}]}
```

Detection needs no component registry. It discovers tracked and unignored
`homeboy.json` files throughout the checkout, including the root. Checkout the
head revision first: manifests, scopes and dependencies are read from disk,
not from the base revision. Deleted components are not deploy targets; deleted
files still select their existing owner. Renames select both old and new owners.

The deepest component root owns each changed path. The root component owns only
unclaimed paths. Effective `scopes.defaults`, `scopes.release`, and `scopes.deploy`
excludes are applied relative to the owner; an excluded nested file never falls
back to its parent. A pattern can be a path, directory, or glob (`docs/**`).
Build-tool ignore files are not interpreted: put detection exclusions in scopes.

Repeat `--shared-path` for repository-relative CI/tooling patterns that should
select **every** component, overriding excludes. Unclaimed files otherwise select
only the root component, or nothing when there is no root component. Shared paths
are explicit workflow configuration; there are no implicit shared directories.

`validation_dependencies` expands the result transitively to same-repo dependents
(including declarations under extension settings). IDs and paths relative to the
declaring component are supported. External dependencies are ignored; cycles
terminate. This lets package changes select their deployable consumers. If a
package itself cannot deploy, configure its deployment/build integration or filter
those entries before the deploy job.

## GitHub Actions: deploy on merge

This example assumes Homeboy, required extensions, SSH credentials, project/server
configuration, and component registrations are provisioned on a trusted self-hosted
deploy runner. Register each component ID against its checkout path (the matrix
`path` is available for setup); detection itself does not register components.
Do not run this deployment workflow on untrusted pull requests.

```yaml
name: Deploy changed components
on:
  push:
    branches: [main]

# Keep deploys from successive merges ordered. Do not cancel an in-flight deploy.
concurrency:
  group: deploy-main
  cancel-in-progress: false

jobs:
  changed:
    runs-on: [self-hosted, homeboy-deploy]
    outputs:
      matrix: ${{ steps.detect.outputs.matrix }}
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.sha }}
          fetch-depth: 0
      - id: detect
        env:
          BEFORE: ${{ github.event.before }}
          SHA: ${{ github.sha }}
        shell: bash
        run: |
          # Normal merges have a real before SHA. For an initial branch push,
          # compare against Git's empty tree using a synthetic empty commit.
          base="$BEFORE"
          if [[ "$base" =~ ^0+$ ]]; then
            tree=$(git hash-object -t tree /dev/null)
            base=$(echo initial | git -c user.name=CI -c user.email=ci@example.com commit-tree "$tree")
          fi
          matrix=$(homeboy component changed --base "$base" --head "$SHA" \
            --shared-path '.github/**' --shared-path 'tooling/**' --format matrix)
          echo "matrix=$matrix" >> "$GITHUB_OUTPUT"

  deploy:
    needs: changed
    if: ${{ toJSON(fromJSON(needs.changed.outputs.matrix).include) != '[]' }}
    runs-on: [self-hosted, homeboy-deploy]
    strategy:
      fail-fast: false
      matrix: ${{ fromJSON(needs.changed.outputs.matrix) }}
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.sha }}
          fetch-depth: 0
          path: ${{ matrix.component }}
      - name: Register component checkout
        env:
          COMPONENT: ${{ matrix.component }}
          COMPONENT_PATH: ${{ matrix.path }}
        run: homeboy component create --local-path "$GITHUB_WORKSPACE/$COMPONENT/$COMPONENT_PATH"
      - name: Deploy merged commit
        env:
          COMPONENT: ${{ matrix.component }}
          SHA: ${{ github.sha }}
        run: homeboy deploy "$COMPONENT" --ref "$SHA"
```

A GitHub Actions concurrency group limits overlap but is not a durable FIFO queue:
GitHub can replace pending runs. Repositories requiring every intermediate commit
to deploy should use a deployment queue. The diff must start at the last deployed
commit if workflows may be skipped; `event.before` describes only that push.

## Output contracts

- `--format matrix`: compact raw JSON `{"include":[{"component":"id","path":"dir"}]}`,
  directly usable by `strategy.matrix`. Empty changes yield `{"include":[]}`.
- `--format json` (default): raw JSON array of those component/path entries.
- `--format ids`: one component ID per line; no output when none changed.

Paths use `/`, root is `.`, and entries are sorted by component ID. Missing refs,
invalid manifests and duplicate IDs are errors, not empty successful results.
Fetch both commits before detection. Concurrent per-component release pushes are
not changed by this command; release serialization/retry remains a separate task.
