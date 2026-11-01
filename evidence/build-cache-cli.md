# Shared build package cache evidence

Primary evidence from the public CLI fixture (real git, one shared package and two consumers):

```text
build-cache e2e: first: shared, one, two = built
build-cache e2e: repeat: shared, one, two = cached
build-cache e2e: unrelated-commit: shared, one, two = cached
build-cache e2e: shared-commit: shared, one, two = built
build-cache e2e: dirty: shared, one, two = built
build-cache e2e: untracked: shared, one, two = built
```

The fixture removes artifacts between cases and checks restored package existence.
Consumers read the built shared package. Recipe execution counters prove exactly
one execution per component on misses and zero on hits, including dirty builds.

Formatting and the local Homeboy lint gate pass. Focused build unit tests pass;
the deploy crate passes with serial test execution. The workspace Homeboy test
gate ran to completion but reported failures outside this change, including
CLI expectations for `--dispatch-provider-id` and `--summary`, worktree inventory
expectations, Cook resource-admission failures, and process timeouts. A parallel
deploy-crate run also exposed shared temporary-filesystem races; serial rerun
passed.

## Outstanding external integration

This checkout does not contain Extra Chill's `rigs/extrachill-network/run.mjs`.
Its reference implementation was read through GitHub, but the consumer's custom
builder has not been removed in this repository and no real Extra Chill gate/deploy
timing comparison has been produced. This implementation uses the existing
`homeboy review build` public primitive. Workspace dependencies currently require
portable component manifests; bare packages without `homeboy.json` are rejected.
Extensions must bump recipe identity when changing helpers not directly declared
as build/pre-build scripts.
