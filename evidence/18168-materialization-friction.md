# #18168: materialization calls misclassified as reruns

## Lab records

Read the issue and its comments (none), plus #18152 and its comments (none).
Queried the Lab observation store through `runs friction --last 3d`,
`runs friction --last 1d`, `runs list --command-contains 'runner workspace materialization'`,
and `runs show`.

The initial three-day projection contained 638 materialization invocations and
526 alleged immediate reruns. Sample successful records all had `caller_session:
null`, `run_id: null`, and `error_code: null`. The one-day projection contained
640 materialization calls, only 11 nonzero exits, but claimed 529 reruns.
These are bounded projections: the report's 1000-row limit was reached, so they
must not be presented as complete day-long coverage.

Named successful Lab invocation evidence:

- `b71ce733-d3c7-43c3-9856-d0426843f09e`: 2026-11-16T01:24:08.319916093Z
- `f99f633f-57f1-488e-adef-08b99dee8cb5`: 2026-11-16T01:24:09.912768731Z
- `073cb4cf-9697-42c9-ad77-0b3adfaea00e`: 2026-11-16T01:24:12.210711514Z
- `b94b0daa-adfb-434c-abef-897e558e3e17`: 2026-11-16T01:24:24.126952445Z

Each is inspectable with `homeboy runs show <id> --format json` on the Lab.

## Caller and trigger

`workspace/sync/protocol_controller.rs::sync_workspace_batch_once` intentionally
invokes the runner twice: first for cache inventory/capacity planning, then to
transfer missing blobs and commit. `materialization_script` invokes the same
public `runner workspace materialization` endpoint with different arguments.
Different stages also use that endpoint. These are not unconditional retries.
The only retry in `sync_workspace_batch_in_roots` is the bounded refetch after
the corruption marker from #18152; removing the plan call would break the
missing-blob protocol.

The redundant path was in `commands/runs/friction.rs`: adjacency of identical
command paths was counted as retries even after success, and all anonymous
callers shared one `None` key. Thus unrelated stages and successful plan/commit
pairs appeared to be one tight retry loop. Removed that counting path rather
than adding caching or changing workspace isolation. Only an identified caller
repeating the same command/run after a failed invocation counts now. Each counted
rerun includes the preceding failure's durable observation ID.

Reprojecting the live one-day window with the fixed binary produced **zero**
materialization reruns among 675 materialization calls in the bounded 1000-row
window. The sole remaining rerun was `review lint`, with a failure link. This is
an immediate reprojection of existing loop records, not a claim that a new
24-hour soak ran during this change.

## Regression

One public CLI end-to-end test executes actual materialization plan endpoints
with both anonymous and identified callers, repeats a validation refusal, and
queries `runs friction` and `runs show` to resolve the failure link. On the
original code it fails because two successful materialization calls are counted
as reruns. With the fix, successful calls are absent from the retry ranking and
the repeated refusal links to its persisted failure. Existing workspace tests
continue to exercise the corruption-refetch plan/commit retry.
