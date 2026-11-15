# Engineering Rules

How to change Homeboy itself, for human contributors, coding agents, and Cooks.

## Layout

- `crates/homeboy-core` holds the services: daemon, job store, workflows,
  artifacts, extensions, and the Lab offload.
- `crates/homeboy-agents` owns agent tasks and the Cook lifecycle.
- `crates/homeboy-cli` parses commands and maps them to core services. It stays
  thin; see the [architecture cleanup map](architecture-cleanup-map.md).
- `crates/homeboy-lab-runner` runs work on a Lab runner.
- `crates/contracts/` holds wire contracts shared across crates and with
  extensions.
- `tests/core/` holds integration tests compiled into `homeboy-core`.

The two subsystems with the most fixes have their own maintainer guides:
[daemon lifecycle](daemon-lifecycle.md) and
[Cook recovery](cook-recovery.md). Read the
relevant one before changing that code.

## Rules

These come from the bugs that cost the most to find. Most of them were one
question answered in two places that drifted apart.

1. **One owner per decision.** Each question has exactly one function that
   answers it, and every caller asks that function. Status, notifications, and
   the command that acts all read the same answer, so they cannot disagree.
   Before adding a predicate, search for an existing one that answers the same
   question.
2. **Put a rule in the layer that owns the data.** A caller that filters or
   reinterprets what an owner returns is a second copy of the rule. Move the
   rule into the owner instead.
3. **Delete; don't wrap.** Remove superseded code paths, compatibility
   readers, aliases, unused arguments, and wrapper chains in the same change
   that supersedes them. Old builds that no longer run do not need a reader.
4. **Split by responsibility.** When a function grows past a few hundred lines,
   give each responsibility its own module with one entry point, and give a
   repeated shape one helper.
5. **Generic core.** Homeboy core stays product-agnostic. Ecosystem behavior
   belongs in extensions, manifests, and hooks.

## Proving a change

- A fix comes with a test that fails before the fix and passes after it,
  through the public interface. Pure refactors show the same test results
  before and after.
- Cover each behavior once. Prefer one end-to-end check over many unit tests
  that restate the implementation.
- Run `cargo fmt --all` before pushing.
- PR descriptions state the problem, the change, and the evidence. Work done
  with AI assistance says so in the PR.
