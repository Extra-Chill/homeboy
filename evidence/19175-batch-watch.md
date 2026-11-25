# Cook-batch status/watch regression evidence

The focused watch regression drives the production polling/enrichment path with two three-child controller snapshots. The initial snapshot has two succeeded children and one pending admission; the second is all-terminal. This is deterministic fixture evidence, **not a live three-Cook end-to-end run**.

Captured progress:

```text
wf-cook-batch-watch counts={"candidate_recoverable":0,"failed":0,"gating":0,"partial":0,"pending_admission":1,"queued":0,"running":0,"succeeded":2} all-terminal=false "repo#0":"succeeded" "repo#1":"succeeded" "repo#2":"pending_admission"
wf-cook-batch-watch counts={"candidate_recoverable":0,"failed":0,"gating":0,"partial":0,"pending_admission":0,"queued":0,"running":0,"succeeded":3} all-terminal=true "repo#0":"succeeded" "repo#1":"succeeded" "repo#2":"succeeded"
```

The separate lifecycle regression advances an actual three-child workflow state machine with `max_parallel: 2`, checks that every declared child appears before admission, and resolves the third child's direct status through its parent without querying a nonexistent workflow. Cancellation preserves never-started reasons. Phase mapping tests cover queued, running, gating, recoverable candidates, success, failure and partial failure.

A live three-Cook batch has not been launched from this checkout. The available controller is shared and already has active jobs; it runs an older installed binary, not this candidate. This evidence does not satisfy the issue's explicit live-batch acceptance requirement.
