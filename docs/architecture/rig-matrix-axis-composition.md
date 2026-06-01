# Rig Matrix Axis Composition

Design for deriving rig variants from one base rig plus explicit axis overlays.

Status: design proposal. `homeboy rig matrix` and `--matrix` selectors are not
part of the current CLI surface.

Tracked by Extra-Chill/homeboy#1844.

## Problem

Rigs currently model one fully-materialized environment each. That is the right
shape for a single environment, but it creates duplicated specs when a benchmark
needs to vary independent dimensions.

Studio/BFB benchmarking already has four natural axes:

- **Agent runtime:** SDK or PI
- **Conversion substrate:** none or BFB
- **Tool policy:** normal, no-browser, or no-validation
- **Profile:** substrate, agentic, or full-site

Hand-authoring each combination would produce a growing set of nearly-identical
rig specs:

```text
studio-agent-sdk
studio-agent-pi
studio-bfb
studio-pi-bfb
studio-pi-bfb-no-browser
studio-pi-bfb-no-validation
```

That is the drift class rigs are meant to remove. The shared pieces should live
once, while axis-specific changes stay small, named, and reviewable.

## Current Shape

The existing rig and bench primitives already provide useful constraints:

- Rig specs are flat JSON files installed at `~/.config/homeboy/rigs/<id>.json`.
- `homeboy rig install` links package specs into that flat config directory and
  records source metadata with `package_path`, `rig_path`, and `discovery_path`.
- Package-relative paths already work through `${package.root}` for rig-owned
  bench workloads.
- `homeboy bench --rig a,b` is cross-rig comparison, not axis expansion.
- `bench.components` is component fan-out inside one rig, not environment
  variant fan-out.
- `RigStateSnapshot` records `rig_id`, `captured_at`, and component git state in
  bench output.
- Resource leases key active-run guardrails off the rig id plus expanded
  resources.

The new primitive should compose with those surfaces instead of replacing them.

## Minimal V1 Proposal

Add an optional top-level `matrix` object to a normal `RigSpec`. A matrix rig is
still a normal rig: running it without matrix selection uses the base spec.

Each axis declares ordered variants. Each variant is a patch array applied to a
clone of the base spec.

```jsonc
{
  "id": "studio-agent",
  "description": "Studio agent benchmark base rig",
  "components": {
    "studio": {
      "path": "~/Developer/studio@agent-sdk-baseline",
      "branch": "agent-sdk-baseline",
      "extensions": { "nodejs": { "studio_bench_variant": "sdk" } }
    }
  },
  "bench": { "default_component": "studio" },
  "bench_workloads": {
    "nodejs": [
      { "path": "${package.root}/bench/studio-agent-runtime.bench.mjs" }
    ]
  },
  "matrix": {
    "axes": {
      "agent_runtime": {
        "default": "sdk",
        "variants": {
          "sdk": { "patch": [] },
          "pi": {
            "patch": [
              { "op": "replace", "path": "/components/studio/path", "value": "~/Developer/studio@pi-runtime-candidate" },
              { "op": "replace", "path": "/components/studio/branch", "value": "pi-runtime-candidate" },
              { "op": "replace", "path": "/components/studio/extensions/nodejs/studio_bench_variant", "value": "pi" }
            ]
          }
        }
      },
      "conversion": {
        "default": "none",
        "variants": {
          "none": { "patch": [] },
          "bfb": {
            "patch": [
              { "op": "add", "path": "/components/block-format-bridge", "value": {
                "path": "~/Developer/block-format-bridge@refresh-expanded-h2bc",
                "branch": "refresh-expanded-h2bc"
              } },
              { "op": "add", "path": "/components/studio/extensions/nodejs/studio_bfb_plugin_path", "value": "~/Developer/block-format-bridge@refresh-expanded-h2bc" },
              { "op": "add", "path": "/resources/exclusive/-", "value": "studio-agent-bfb-bench" },
              { "op": "add", "path": "/resources/paths/-", "value": "${components.block-format-bridge.path}" },
              { "op": "add", "path": "/bench_workloads/nodejs/-", "value": { "path": "${package.root}/bench/studio-bfb-write-path.bench.mjs" } }
            ]
          }
        }
      }
    }
  }
}
```

V1 uses RFC 6902-style JSON Patch operations (`add`, `replace`, `remove`) with
JSON Pointer paths. This is verbose, but it is precise, deterministic, and
avoids inventing Homeboy-specific merge semantics for every JSON shape.

## Why Patch Arrays

Patch arrays win over nested overlays, dot-path assignment, and free-form deep
merge for this feature.

| Shape | Problem |
|---|---|
| Nested overlay objects | Ambiguous list semantics: replace list, append item, merge by `id`, or dedupe? |
| Dot-path assignment | Cannot cleanly address list append/remove; escaping dotted keys is awkward. |
| Deep merge | Silent object/list behavior becomes policy; conflicts are hard to explain. |
| JSON Patch | Explicit operations, standard pointer syntax, good validation errors. |

The cost is verbosity. That is acceptable because matrix variants should be
small, and package authors can still keep full hand-authored rigs when a variant
needs many changes.

## CLI Surface

Use two surfaces: one for single derived variants and one for cartesian
expansion.

```text
# Single derived rig for normal rig verbs.
homeboy rig check studio-agent --variant agent_runtime=pi --variant conversion=bfb
homeboy rig up studio-agent --variant agent_runtime=pi --variant conversion=bfb
homeboy rig status studio-agent --variant agent_runtime=pi --variant conversion=bfb

# Bench comparison across an axis product.
homeboy bench studio --rig studio-agent \
  --matrix agent_runtime=sdk,pi \
  --matrix conversion=none,bfb

# Proposed plan inspection without running anything.
homeboy rig matrix studio-agent --matrix agent_runtime=sdk,pi --matrix conversion=none,bfb
homeboy rig matrix studio-agent --variant agent_runtime=pi --variant conversion=bfb --json
```

Rules:

- `--variant axis=value` selects exactly one value for one axis.
- `--matrix axis=a,b,c` selects multiple values and expands to the cartesian
  product.
- Missing axes use their declared `default` value.
- Unknown axes or values are validation errors.
- `rig up`, `rig check`, `rig down`, `rig status`, and `rig sync` accept only
  `--variant`, not multi-value `--matrix`, because they operate on one
  environment at a time.
- `homeboy bench --rig <id> --matrix ...` expands derived rig entries and feeds
  them through the existing cross-rig comparison envelope.
- `homeboy bench --rig a,b --matrix ...` is rejected in v1. The matrix belongs
  to one base rig at a time; mixing explicit cross-rig lists with cartesian
  expansion is a later design if a real use case appears.

For agent-runner fan-out, matrix expansion should also be representable as a
generic `core::agent_task` plan. The plan-only shape is
`homeboy/agent-task-matrix-plan/v1`: each cartesian cell becomes an
`AgentTaskRequest` with stable `task_id`, `parent_plan_id`, `group_key`, and
matrix axis metadata. Once the generic scheduler and executor adapter contracts
from #3208 and #3209 exist, that same plan is the handoff boundary for executing
cells without adding a bench-specific scheduler.

## Derived Rig Identity

Derived rigs are ephemeral values, but they need stable ids for output, state,
leases, and logs.

Canonical id format:

```text
<base>[<axis>=<value>,<axis>=<value>]
```

Axis names are sorted by declaration order in the spec, not input order. Values
are slug-validated using the same safe-id vocabulary as rig ids. Example:

```text
studio-agent[agent_runtime=pi,conversion=bfb]
```

For filesystem paths, use a sanitized derived id:

```text
studio-agent--agent_runtime-pi--conversion-bfb.state/
studio-agent--agent_runtime-pi--conversion-bfb.json  # lease file only, not a config spec
```

The human-facing id stays bracketed because it is more readable in bench output.
The path-safe id is an implementation detail.

## Runtime Materialization

V1 should materialize derived rigs in memory only:

```text
load base rig
    |
    v
clone RigSpec
    |
    v
apply selected variant patches in axis declaration order
    |
    v
validate as a normal RigSpec
    |
    v
set spec.id = canonical derived id
    |
    v
run existing rig/bench code
```

No generated config files are written under `~/.config/homeboy/rigs/` in v1.

Reasons:

- It keeps `rig install` and `rig update` unchanged: packages install one base
  spec, not N generated files.
- It prevents stale generated variants after package updates.
- It keeps review diffs small: package authors review base + axes, not generated
  output.
- It matches how `bench --rig a,b` already treats rig specs as inputs to one
  command invocation.

Generated files can be a later `homeboy rig matrix materialize` convenience, not
the core execution model.

## Merge Semantics

Patch application is deterministic:

1. Start with the loaded base rig after filename-derived id normalization.
2. Determine the full axis selection, filling missing axes from `default`.
3. Apply axes in declaration order from `matrix.axes`.
4. Apply each selected variant's `patch` array in order.
5. Validate the final JSON by deserializing it back into `RigSpec`.
6. Set the runtime `id` to the canonical derived id.

Conflict handling is intentionally strict:

- `replace` requires the target path to exist.
- `remove` requires the target path to exist.
- `add` to an object member fails if that member already exists unless the path
  targets an array append with `/-`.
- Array append uses RFC 6902 `/-`.
- There is no implicit dedupe of lists.
- If two axes mutate the same scalar path, the later axis wins only when both
  operations are explicit and valid. V1 should emit a warning in the matrix plan;
  a future phase can make this an error if real specs need stronger guarantees.

This keeps the rule simple: the patch says exactly what happened.

## Validation

Validation should happen in two layers.

First, validate matrix declarations while loading the base rig:

- Axis names and variant names must be slug-safe.
- Each axis with variants must declare a default.
- The default must name an existing variant.
- Patch paths must be valid JSON Pointers.
- Patch operations must be one of `add`, `replace`, or `remove` in v1.

Second, validate every derived rig that will run:

- Apply patches.
- Deserialize the result as `RigSpec`.
- Reuse existing rig validation surfaces by running the requested command's
  normal preflight (`rig check` before bench, pipeline ordering checks during
  pipeline execution, resource expansion before leases).
- `homeboy rig matrix` should apply and validate every requested combination
  without executing pipelines.

Example errors:

```text
Invalid matrix selection for rig 'studio-agent': axis 'conversion' has no variant 'blockify'.
Available variants: none, bfb
```

```text
Invalid matrix patch in rig 'studio-agent' axis 'agent_runtime=pi' patch[0]:
replace path '/components/studio/path' does not exist.
```

```text
Invalid derived rig 'studio-agent[agent_runtime=pi,conversion=bfb]':
component 'block-format-bridge' declares no path.
```

The error should name the base rig, axis, variant, patch index, operation, and
path whenever possible. Those fields matter more than the raw serde error.

## State And Bench Metadata

Derived rig selections should appear in both rig state and bench metadata.

Extend `RigStateSnapshot` with optional matrix metadata:

```jsonc
{
  "rig_id": "studio-agent[agent_runtime=pi,conversion=bfb]",
  "base_rig_id": "studio-agent",
  "matrix": {
    "agent_runtime": "pi",
    "conversion": "bfb"
  },
  "captured_at": "2026-04-27T12:00:00Z",
  "components": { "studio": { "path": "...", "sha": "...", "branch": "..." } }
}
```

The same metadata should be available to bench runners as environment variables:

```text
HOMEBOY_RIG_ID=studio-agent[agent_runtime=pi,conversion=bfb]
HOMEBOY_RIG_BASE_ID=studio-agent
HOMEBOY_RIG_MATRIX_JSON={"agent_runtime":"pi","conversion":"bfb"}
```

The bench output should keep the current cross-rig envelope. Each expanded
variant becomes one `RigBenchEntry` with its own `rig_id` and `rig_state`.

## Resource Leases

Use the canonical derived rig id for the lease's `rig_id`. This prevents two
commands from mutating the same derived rig simultaneously, while still letting
resource declarations catch conflicts across sibling variants.

Example:

```jsonc
{
  "resources": {
    "exclusive": ["studio-agent-bfb-bench"],
    "paths": ["${components.studio.path}", "${components.block-format-bridge.path}"],
    "process_patterns": ["wordpress-server-child.mjs"]
  }
}
```

The `conversion=bfb` variant can add those resources. Then these conflict:

```text
studio-agent[agent_runtime=sdk,conversion=bfb]
studio-agent[agent_runtime=pi,conversion=bfb]
```

They share the BFB worktree and Studio daemon process pattern. The existing
lease overlap code already catches that once resources are present on the
derived spec.

Variants that only change isolated component paths can run concurrently if their
resources do not overlap.

## Package Install And Update

Matrix rigs should remain source-package data, not install-time generated files.

For `homeboy rig install <repo.git//subpath>`:

- Discovery still finds normal `rig.json` files.
- A rig with `matrix` is installed as one rig id.
- Source metadata still points at the base `rig_path` and package root.
- `${package.root}` continues to resolve from that source metadata when derived
  variants run.

For `homeboy rig update`:

- Update pulls the package and refreshes the base linked config.
- No generated variant files need refresh.
- A changed matrix axis is immediately reflected on the next command.

This avoids another lifecycle registry. The package root is still the only
source of truth.

## Studio Worked Example

The current hand-authored package has three related rigs:

- `studio-agent-sdk`: SDK runtime baseline.
- `studio-agent-pi`: PI runtime candidate.
- `studio-bfb`: BFB conversion substrate experiment.

The matrix shape can collapse them into one base rig:

```jsonc
{
  "id": "studio-agent",
  "description": "Studio agent benchmark matrix",
  "components": {
    "studio": {
      "path": "~/Developer/studio@agent-sdk-baseline",
      "branch": "agent-sdk-baseline",
      "extensions": {
        "nodejs": { "studio_bench_variant": "sdk" }
      }
    }
  },
  "services": {
    "studio-daemon": {
      "kind": "external",
      "discover": { "pattern": "wordpress-server-child.mjs" }
    }
  },
  "bench": { "default_component": "studio" },
  "bench_workloads": {
    "nodejs": [
      { "path": "${package.root}/bench/studio-agent-runtime.bench.mjs" }
    ]
  },
  "pipeline": {
    "up": [
      { "kind": "command", "label": "Install Studio dependencies", "cwd": "${components.studio.path}", "command": "npm install" },
      { "kind": "command", "label": "Build Studio CLI", "cwd": "${components.studio.path}/apps/cli", "command": "npm run build --silent" },
      { "kind": "service", "id": "studio-daemon", "op": "stop" }
    ],
    "check": [
      { "kind": "check", "label": "Studio CLI eval runner built", "file": "${components.studio.path}/apps/cli/dist/cli/eval-runner.mjs" },
      { "kind": "check", "label": "Studio package dependencies installed", "file": "${components.studio.path}/node_modules" }
    ],
    "down": [
      { "kind": "service", "id": "studio-daemon", "op": "stop" }
    ]
  },
  "matrix": {
    "axes": {
      "agent_runtime": {
        "default": "sdk",
        "variants": {
          "sdk": { "patch": [] },
          "pi": {
            "patch": [
              { "op": "replace", "path": "/components/studio/path", "value": "~/Developer/studio@pi-runtime-candidate" },
              { "op": "replace", "path": "/components/studio/branch", "value": "pi-runtime-candidate" },
              { "op": "replace", "path": "/components/studio/extensions/nodejs/studio_bench_variant", "value": "pi" }
            ]
          }
        }
      },
      "conversion": {
        "default": "none",
        "variants": {
          "none": { "patch": [] },
          "bfb": {
            "patch": [
              { "op": "add", "path": "/components/block-format-bridge", "value": { "path": "~/Developer/block-format-bridge@refresh-expanded-h2bc", "branch": "refresh-expanded-h2bc" } },
              { "op": "add", "path": "/components/studio/extensions/nodejs/studio_bfb_plugin_path", "value": "~/Developer/block-format-bridge@refresh-expanded-h2bc" },
              { "op": "add", "path": "/bench_workloads/nodejs/-", "value": { "path": "${package.root}/bench/studio-bfb-write-path.bench.mjs" } },
              { "op": "add", "path": "/resources/exclusive/-", "value": "studio-agent-bfb-bench" },
              { "op": "add", "path": "/resources/paths/-", "value": "${components.block-format-bridge.path}" },
              { "op": "add", "path": "/resources/process_patterns/-", "value": "wordpress-server-child.mjs" }
            ]
          }
        }
      }
    }
  }
}
```

Then the common bench calls become:

```bash
# SDK vs PI, without BFB.
homeboy bench studio --rig studio-agent \
  --matrix agent_runtime=sdk,pi \
  --variant conversion=none

# All SDK/PI x none/BFB combinations.
homeboy bench studio --rig studio-agent \
  --matrix agent_runtime=sdk,pi \
  --matrix conversion=none,bfb

# Bring up one derived environment for manual inspection.
homeboy rig up studio-agent --variant agent_runtime=pi --variant conversion=bfb
```

The bench comparison output has four `rigs[]` entries:

```text
studio-agent[agent_runtime=sdk,conversion=none]
studio-agent[agent_runtime=sdk,conversion=bfb]
studio-agent[agent_runtime=pi,conversion=none]
studio-agent[agent_runtime=pi,conversion=bfb]
```

## Incremental Implementation Phases

### Phase 1: Types And Plan Only

- Add `matrix` structs to `RigSpec`.
- Add a resolver that turns `--variant` / `--matrix` selections into derived
  rig specs.
- Add `homeboy rig matrix <rig>` to print the plan and validate combinations.
- No changes to `rig up` or `bench` yet.

This phase is useful on its own because package authors can validate the design
against real Studio package specs before runtime commands depend on it.

### Phase 2: Single-Variant Rig Commands

- Add `--variant axis=value` to `rig up`, `rig check`, `rig down`, `rig status`,
  and `rig sync`.
- Use the derived id for state and leases.
- Add matrix metadata to `RigStateSnapshot`.

### Phase 3: Bench Matrix Expansion

- Add `--variant` and `--matrix` to `homeboy bench`.
- Expand one base rig into N derived `RigBenchEntry` values.
- Preserve the current cross-rig output envelope.
- Reject `--baseline` and `--ratchet` for multi-derived comparisons just like
  current cross-rig comparisons.

### Phase 4: Materialize Convenience

- Optional `homeboy rig matrix materialize <rig>` writes generated specs for
  humans or tools that need concrete ids.
- Generated files include source comments/metadata and are always treated as
  derived output, not package source.
- This stays out of v1 until a consumer proves it needs files on disk.

## Migration From Hand-Authored Rigs

Migration should be additive and reversible.

1. Keep existing rigs (`studio-agent-sdk`, `studio-agent-pi`, `studio-bfb`) in
   the package.
2. Add a new matrix rig (`studio-agent`) next to them.
3. Run `homeboy rig matrix studio-agent --matrix ...` and compare derived specs
   against the hand-authored rigs.
4. Move bench workflows to the matrix rig once output matches.
5. Deprecate the hand-authored variants in package docs.
6. Delete hand-authored variants only after the matrix rig has covered the same
   benchmark use cases for at least one release cycle.

No existing rig id changes are required. Existing `homeboy bench --rig
studio-agent-sdk,studio-agent-pi` commands keep working throughout the migration.

## Out Of Scope

- No generated config files in v1.
- No implicit matrix expansion for plain `rig up` or `rig check`; those commands
  operate on one selected variant.
- No nested matrix inheritance between rigs.
- No conditional patches (`if axis A is X and axis B is Y`) in v1. If a
  combination needs special behavior, model it as an explicit third axis or keep
  a hand-authored rig.
- No automatic conflict resolution for patches that touch the same path.
- No statistical changes to bench comparisons.
- No replacement for `bench.components`; component fan-out remains separate from
  environment variant fan-out.
- No replacement for `--rig a,b`; explicit cross-rig comparison remains the
  primitive for unrelated rigs.

## Open Questions

- Should `add` to an existing object key be a hard error in v1, or should JSON
  Patch's standard replace-like behavior be preserved? This design recommends a
  Homeboy-specific hard error because accidental overwrites are more dangerous
  than verbosity.
- Should `homeboy rig list` show matrix-capable rigs with an axis count? Useful,
  but not required for v1.
- Should bench baselines key by derived rig id or by base rig plus matrix map?
  They are equivalent if canonicalization is stable; storing both may make
  future migrations easier.
