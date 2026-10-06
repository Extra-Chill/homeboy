# Active task ownership by caller

Coding runtimes can associate work with an opaque caller reference through
`HOMEBOY_CALLER_CONTEXT`. Cook captures it into the existing `client_context`
before dispatch; native submission freezes it in the original controller plan
and durable run. An explicit `client_context.caller_context` takes precedence.
The reference is routing metadata, not a permission grant.

Controller workspace resolution publishes the authoritative repository and
checkout in the existing plan/run `caller_workspace` projection. This is kept
separate from a runner's execution directory and persists across transport.
An original run's caller association cannot be reassigned by a later update.

```sh
homeboy agent-task active-scope --context opaque-caller-reference
```

The response data uses `homeboy/agent-task-active-scope/v1` with:

- `caller_context`: the requested reference;
- `workspaces`: unique `{ repository, working_directory, run_ids }` owners;
- `pending_run_ids`: admitted work without a proved controller checkout.

The read opens the authoritative observation store read-only. An expression
index projects only the caller, repository, checkout and run ID for queued or
running lifecycle records. Lookup is an exact indexed predicate with a fixed
32-owner bound; overflow is an error. It reads no conversations, task prompts,
historical run files or independent session/task store. Terminal lifecycle
updates remove their ownership from the active index in the same transaction.

Consumers must report multiple checkout owners or pending allocation rather
than choosing a latest row or an unrelated component root. Historical tasks and
ordinary maintenance commands create no active ownership candidates.

After upgrading, normal task admission initializes the new observation schema
and its ownership index. Reads against an unmigrated existing store fail rather
than falling back to a history scan.
