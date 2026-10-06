# Cook driver runtime proof — #15566 / #15568

The per-Cook kernel lock fences the actual Cook runtime entry point. Contention
occurs before recipe or provider work, and the OS releases ownership when its
holder dies. The PID file is diagnostic evidence, not ownership authority.

## Reproduction against the untouched production base

Production base: `68ba10305`. Only the new test fixture was overlaid onto an
isolated baseline checkout; production code was unchanged.

```sh
cargo nextest run -p homeboy-agents -p homeboy-cli -p homeboy-lab-runner \
  -E 'test(=agent_task_service::cook::tests::a_foreign_kernel_owner_blocks_cook_before_recipe_or_provider_work)'
```

Nextest `cf5a815a-f866-4926-aa34-b53f6cfbb547` failed: the supposedly foreign-owned
Cook dispatched provider work and returned `in_flight`. This test targets an
existing runtime entry point and can distinguish the original failure from the
candidate behavior.

## Candidate verification

Ownership head `3d78bb12c` plus runtime-proof changes was verified in an isolated
Lab checkout on the default test stack:

```sh
cargo fmt --all --check
cargo nextest run -p homeboy-agents -p homeboy-cli -p homeboy-lab-runner --no-fail-fast \
  -E 'test(cook_driver_tests) | test(concurrent_first_cooks_elect) | test(foreign_kernel_owner) | test(cook_recipe::tests) | test(terminal_cook_continuation)'
```

Nextest `9fdf207c-5905-4878-8360-862b90061c0d`: **63 passed**, zero failures. One
entry is the subprocess probe helper. Evidence covers:

- actual runtime admission refusal while a foreign file description owns the lock;
- zero recipe creation, run creation, and provider dispatch during contention;
- successful admission and one dispatch after the foreign owner releases;
- two controllers racing while the first is paused at the provider boundary;
- preservation of the owner's recipe and lifecycle plan;
- a real child process killed while holding ownership, followed by immediate
  ownership recovery without changing its stale PID record;
- durable continuation queue, claim, restart, and reconstruction behavior.

## Concurrency fixture repair

The old recipe-creation barrier required two runtime controllers to enter recipe
creation. Exclusive driver admission correctly prevents the second from doing
so; the integrated candidate consequently timed out in that fixture. The barrier
and its in-code test hook were retired. The replacement pauses the admitted
driver at its provider boundary and proves the contender receives retryable
contention without changing durable inputs or dispatching a second provider.

The original three candidates (#15563, #15569, #15568) were also integrated onto
the same main base. With the corrected fixture and process proof, Nextest
`23c2d3d4-a7b4-435d-92e2-2f458bce5e3b` passed all 23 selected entries.

This establishes ownership fencing; the canonical atomic lineage-writer work in
#15567 remains separate. Authoritative CI must verify the published head.

## Execution and AI provenance

Direct Lab verification outside Homeboy orchestration was authorized for this
program. OpenAI `openai/gpt-6.1-sol` via OpenCode, directed by Chris Huber, reviewed
the existing candidates, reproduced the fixture conflict and baseline admission
failure, implemented stronger runtime tests, and ran verification. The original
ownership implementation came from the existing PR, not this verification pass.
