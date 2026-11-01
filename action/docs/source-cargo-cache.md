# Source Cargo cache reuse (#545)

The source resolver tries an exact compatibility/lock/tree key, then a
compatibility/lock prefix, then compatibility with any lock. Cargo validates
restored artifacts before use. Compiler, profile, flags, Cargo configuration,
source location and runner image remain compatibility boundaries.

The composite action and reusable CI restore on every ref and save after a
successful build unless the exact key already hit. PR saves are kept: a PR's
later pushes restore its own merge-ref cache, and the consumer CI is PR-only,
so a default-branch-only save would write nothing today. Merge-ref caches
still cannot warm *other* PRs; that needs the default-branch seeding below.

## Consumer follow-up: Extra-Chill/homeboy

Inspection of the consumer's default-branch workflows via GitHub's contents
API found these differences:

- `.github/workflows/release.yml`, `gate-build`: Ubuntu 22.04, Rust 1.95.0,
  release profile, canonical resolver pinned at
  `8194050639aeb51728460051884f183aa74eade5`.
- `.github/workflows/ci.yml`: PR-only (plus workflow dispatch), source `.`,
  default release profile and stable Rust through reusable CI on ubuntu-latest.
- Release planning separately uses the **dev** profile, intentionally isolated.

The release and PR build steps do not declare differing `CARGO_PROFILE_*` or
`CARGO_TARGET_*` overrides. Runner `ImageOS` alone partitions the release and
PR caches; compiler differences can partition them too. Do not remove the
image/compiler boundary merely to force a hit: older release consumers require
the Ubuntu 22.04 GLIBC baseline.

To seed shared PR-compatible caches without changing the release ABI, add a
push trigger for the default branch to the consumer CI workflow so its binary
preparation runs with the same inputs, runner and toolchain as PR CI. Update
its action/workflow pins to this change. Alternatively run the existing CI
workflow_dispatch on the default branch to seed a compatible entry. Keep the
release gate's older-runtime build separate. This repository cannot change
those consumer workflow files or their pinned action revisions.

This is a partial fix: the restore fallback and redundant-save skip are
implemented and tested here; cross-PR reuse needs that consumer update.
Also, `Cargo.toml` is currently hashed as configuration, so releases that change
its workspace version still change compatibility even with the broader lock
fallback. Normalizing that manifest requires distinguishing dependency/profile
changes from version-only changes and is not implemented here.
