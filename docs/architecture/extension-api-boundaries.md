# Extension API Boundaries

Homeboy talks about "the extension API" as one thing. It is three, and only one
of them is designed. This document names all three, assigns every
`ExtensionManifest` field to one, and states the rule each boundary should
follow.

It is a map, not a plan. Nothing here proposes moving code.

## Measured surface

Taken from `4560bcaaf`:

| | |
|---|---|
| `ExtensionManifest` fields | 48 |
| `ExtensionManifest` accessors | 50 |
| `homeboy-extension-contract` | 54 modules, 7,334 lines |
| `ExtensionCapability` variants | 8 |
| `ExtensionRunner` call sites | 18 |
| Shell helpers in `homeboy-extension/src/runtime/` | 14 |
| Crates referencing `ExtensionManifest` outside the contract crate | 9, across 66 files |

The ratio that matters: **8 of 47 classified fields are reached through the
designed capability contract. 27 are read directly by named subsystems.**

## The three boundaries

### B1 — Executable capability

An extension declares a script; Homeboy runs it with resolved settings, a run
dir, the env-provider chain, invocation requirements, structured sidecars, and
redaction.

This boundary is designed. `ExtensionCapability::descriptor` centralizes label,
manifest-support probe, and script accessor, so adding a capability is one arm
rather than parallel edits across every consumer. Dispatch is generic:
`has_manifest_support` and `script_path` are called through the enum, not
through field access. `ExtensionCapability::Trace` and `::Audit` have **no**
direct field reads outside the contract crate at all — the descriptor is their
only route in. That is the encapsulation working.

Two shapes exist within it:

- **Owner-elected** — `resolve_extension_for_capability` picks exactly one
  provider, using `composition.includes` to break ties. Lint, Test, Build,
  Bench, Fuzz, Trace, Deps.
- **Aggregating** — the result is the union of every linked extension's
  contribution, so there is no owner to elect. Audit reference paths, via
  `resolve_execution_context_for_extension` (#13723).

**Rule:** anything Homeboy *executes* belongs here, and reaches the manifest
only through a capability descriptor.

### B2 — Declarative config

Homeboy does not execute anything. A core subsystem reads extension-declared
data and acts on it: deploy policy, CI job specs, artifact cleanup, release
preflights, agent runtimes, notification transports, toolchain readiness.

This boundary is undesigned. It is not wrong — the data is typed, versioned in
places, and largely well-factored into `manifest_*` modules. But it has no
stated membership rule, so it grew by accretion: every subsystem that needed
something from an extension added a field and read it directly. Nine crates now
reach into manifest structure.

Coupling is uneven. Most fields have exactly one reading crate; a few are read
by three or four (`runtime`, `deploy`, `build`, `agent_runtimes`). The
multi-reader fields are where an extension author's mental model breaks, because
changing one field's meaning requires knowing which unrelated subsystems consult
it.

**Rule to decide:** B2 has none today. That is the actual gap this document
exists to surface.

### B3 — Shell runtime contract

The 14 helpers in `homeboy-extension/src/runtime/` — `settings.sh`,
`runner-prelude.sh`, `emit-lint-finding.sh`, `emit-test-failure.sh`,
`sidecar-writer.sh`, `resolve-context.sh`, `write-test-results.sh`,
`bench-helper.{sh,mjs}`, `command-capture.sh`, `failure-trap.sh`,
`bash-preflight.sh`, `disposable-local-db.sh`, `runner-steps.sh`.

This is what extension authors actually program against day to day. It is bash,
and it is **versioned by nothing**. Homeboy declares core-compatibility ranges
for manifests (`core_compat`), but a runtime helper's calling convention can
change without any declared contract version.

Known consequences are already filed against `homeboy-extensions`: a six-line
bootstrap preamble duplicated across 14 runners because there is no supported way
to locate the helper directory (Extra-Chill/homeboy-extensions#2508), and two
competing `settings.sh` implementations whose selection depends on an
environment variable, so Lab and local can silently drift
(Extra-Chill/homeboy-extensions#2505).

**Rule:** B3 is a published interface and should carry a version, because
extensions are separately released artifacts.

### B0 — Identity and packaging

`id`, `name`, `version`, `icon`, `description`, `author`, `homepage`,
`source_url`, `provides`, `composition` / `includes`, `extension_path`, `extra`.

Not a behavior boundary. Included so the field map is exhaustive.

`extra` is explicitly a forward-compatibility buffer rather than an extension
point, and its doc comment carries the reasoning. It has two remaining readers:
the legacy camelCase `sourceUrl`, and `recipe_run_providers` (#13724).
`deployment_providers` was the third and became typed in #13723.

## Field map

Reader crates were resolved by locating direct field reads and accessor call
sites outside `homeboy-extension-contract`. Fields showing no direct read were
checked individually; every one is reached through an accessor or through the
capability descriptor. **There are no inert typed fields.**

### B1 — Executable capability (8)

| Field | Shape | Reached via |
|---|---|---|
| `lint` | owner-elected | descriptor + `homeboy-extension` |
| `test` | owner-elected | descriptor + `homeboy-extension` |
| `build` | owner-elected | descriptor + `homeboy-core`, `homeboy-deploy`, `homeboy-extension` |
| `bench` | owner-elected | descriptor + `homeboy-cli` |
| `fuzz` | owner-elected | descriptor + `homeboy-cli`, `homeboy-extension` |
| `trace` | owner-elected | descriptor only |
| `deps` | owner-elected | descriptor + `homeboy-core` |
| `audit` | aggregating | descriptor only |

`build` is the outlier: a capability whose data is also read directly by deploy
and core. It is the clearest single instance of B1/B2 overlap.

### B2 — Declarative config (27)

| Field | Reading crates |
|---|---|
| `runtime` | `homeboy-cli`, `homeboy-code-audit`, `homeboy-core`, `homeboy-extension` |
| `deploy` | `homeboy-core`, `homeboy-deploy`, `homeboy-extension` |
| `agent_runtimes` | `homeboy-agents`, `homeboy-core`, `homeboy-extension` |
| `ci` | `homeboy-cli`, `homeboy-core` |
| `cli` | `homeboy-cli`, `homeboy-core` |
| `settings` | `homeboy-cli`, `homeboy-core` |
| `requires` | `homeboy-cli`, `homeboy-core` |
| `notification_transports` | `homeboy-cli`, `homeboy-core` |
| `executable` | `homeboy-cli`, `homeboy-extension` |
| `contract_producers` | `homeboy-cli`, `homeboy-extension` |
| `actions` | `homeboy-extension`, `homeboy-release` |
| `scripts` | `homeboy-extension` |
| `env_provider` | `homeboy-extension` |
| `structured_sidecars` | `homeboy-extension` |
| `source_snapshot` | `homeboy-core` |
| `diagnostics` | `homeboy-core` |
| `artifact_cleanup` | `homeboy-core` |
| `external_storage_retention` | `homeboy-core` |
| `hooks` | `homeboy-core` |
| `composition` | `homeboy-core` |
| `component_env` | `homeboy-cli` |
| `external_check_detail_resolvers` | `homeboy-cli` |
| `materialization_source` | `homeboy-cli` |
| `platform` | `homeboy-cli` (via `database()`) |
| `toolchain_readiness` | `homeboy-lab-runner` |
| `autofix_verify` | `homeboy-refactor` |
| `release_preflights` | `homeboy-release` |
| `agent_task` | `homeboy-agents` (via `default_backend_from_policy_sources`) |

## What the map predicts

**Adding a capability is cheap; adding declarative config is free.** B1 has a
descriptor that forces one arm and a compile error when you forget. B2 has no
gate at all — a new field costs one line and creates a permanent coupling from a
named crate into manifest structure. Cost asymmetry, not carelessness, is why B2
is 27 fields and B1 is 8.

**"Is this the extension API?" has no answer today.** An extension author sees
one JSON file and one set of shell helpers. Homeboy sees a generic dispatch
contract, twenty-seven private agreements, and an unversioned shell interface.
Both views are correct, which is the problem.

**Symptom fixes will keep recurring.** #13723 moved audit from a bespoke
execution path into B1 and deploy providers from `extra` into B2. Both were real
fixes. Neither reduced the number of boundaries, and nothing prevents the next
subsystem from adding a twenty-eighth private agreement.

## Questions this document exists to provoke

1. **Should B2 have a membership rule?** Candidates: every field declares its
   reading subsystem; or B2 fields are reached through a typed per-subsystem view
   rather than the whole manifest; or B2 is accepted as-is and simply documented.
2. **Should B3 carry a version?** Extensions ship separately from Homeboy, and
   `core_compat` covers manifests but not helper calling conventions.
3. **Is `build` in the right boundary?** It is a capability that three crates
   also read directly.
4. **Is "aggregating vs owner-elected" a permanent distinction in B1,** or is
   audit the only member it will ever have?

## Reproducing the counts

```sh
# Manifest fields and accessors
grep -cE '^\s+pub [a-z_]+:' crates/contracts/homeboy-extension-contract/src/manifest.rs
grep -cE '^\s+pub fn '      crates/contracts/homeboy-extension-contract/src/manifest.rs

# Crates coupled to the manifest outside the contract crate
grep -rl ExtensionManifest crates --include='*.rs' \
  | grep -v contracts/homeboy-extension-contract \
  | sed 's|crates/contracts/||;s|crates/||;s|/src/.*||' | sort | uniq -c | sort -rn
```

Counts drift. Re-run before citing them.
