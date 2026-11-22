# Lab capacity admission

A runner's `effective_budget` is its configured `concurrency_limit`. When unset,
it defaults to one slot per logical CPU (`available_parallelism`, or one if
unavailable). Capacity observations and the step queue report the same budget.
The first `/proc/loadavg` value remains diagnostic only: the runner's own jobs
already occupy slots and must not subtract from capacity a second time. Host
overload refusal uses the existing PSI pressure thresholds (`admission_reason:
pressure`), independently of slot capacity. Existing work is never preempted.

Heavy execution and workflow steps take a slot before launching. A step's owner
is its workflow instance (`root_durable_run_id`), else its linked run. Waiters are
ordered by lane, then by the owner with the fewest active slots, then by earliest
arrival.
A waiting step polls actively and releases its ticket on cancellation. Launcher
steps do not hold a slot while waiting for children. By default, direct runner job
reservations keep their configured concurrency limit unchanged.

### Priority lanes

A step declares its lane with `HOMEBOY_LAB_PRIORITY=high|normal` in its public
environment (default `normal`). `homeboy agent-task cook --priority high` sets it
for every step the Cook admits; `agent-task fanout cook-batch --priority high`
passes it to every child Cook. Any `HOMEBOY_*` value in the caller environment
travels the same way. When a slot frees, `high` waiters are admitted before
`normal` waiters. Within a lane, the existing least-occupied-owner then FIFO
order applies. A Cook waiting on the controller for a runner (`capacity_reached`)
is ranked the same way against other waiters for that runner on the same
controller. A free runner slot goes to a `high` waiter first. `high` waiters
re-probe at most every 5 seconds.

### Owner share cap

While another owner is waiting, one owner (for example a single workflow
fan-out) cannot be admitted beyond `HOMEBOY_LAB_OWNER_MAX_SHARE` percent of the
effective budget. Set this in the executing daemon environment (1-100, default
50, never below one slot). An over-share owner is deferred behind every other
waiter, even a higher-lane one. With nobody else waiting it can still fill idle
slots. Running work is never preempted. Cooks waiting on other controllers count
too, through their waiter reservations (below).

Direct runner jobs count toward the share as well. A Lab gate a Cook sends to the
runner and a Cook placed on the Lab carry `HOMEBOY_LAB_OWNER` (the workflow
instance, else the Cook), their lane, and a declared budget in their job
environment: the gate timeout for a gate, the Cook's declared budget for a Cook.
The daemon charges each one holding a child to that owner.
A gate also carries `HOMEBOY_LAB_PARENT_JOB_ID`, the daemon job its Cook runs in.
When that job is a current holder on the same Lab for the same owner, the gate
uses the Cook's slot and is not charged again; its budget still counts toward ETAs.

### Cooks waiting on other controllers

A remote Cook waiting on a Lab runner (`capacity_reached`) registers a waiter
reservation on the Lab daemon with `POST /capacity/waiters`
(`{waiter_id, owner, priority, first_waited_at}`; `release: true` leaves). It renews the
reservation on every eligibility probe, at most 30 seconds apart. A reservation
not renewed within 120 seconds expires, so a lost controller cannot leak queue
entries. `first_waited_at` is when the Cook first started waiting. It sets the
waiter's arrival when it registers, including after an expiry or a controller
restart, and a renewal keeps the registered arrival. A time ahead of the Lab's
clock counts as now. The Lab ranks remote waiters with its own step waiters as one
queue: lane, owner share, then earliest arrival. It replies with the waiter's
Lab-wide position, whether it may proceed, the effective budget, load, and
holders.

At equal lane, a waiter that arrived earlier holds back a later one, on this Lab
or on any controller. The Lab never idles on a remote waiter: it holds back local
steps only while some step holder runs. Once the Lab tells a waiter it may
proceed, the controller re-probes within 5 seconds. A waiter still registered
30 seconds after that (`remote_claim_grace_seconds`) is blocked for another
reason and stops holding others back until it is outranked again. A waiter that
stops heartbeating expires. Older daemons without the route keep the
controller-local ranking.

### Queue position and ETA

Waiting progress reports `waiting_for: lab_capacity`, `queue_position`,
`waiters_ahead`, `active_slots`, `priority`, `owner_slot_cap`,
`load_average_1m`, `effective_budget`, and `eta_seconds` with
`eta_kind: upper_bound_from_declared_budgets`. A step declares its expected run
time with `HOMEBOY_LAB_BUDGET_SECONDS`. When that is unset, `agent-task cook`
declares one attempt's existing limits: the provider wall-clock ceiling
(`--timeout-ms`, extended up to 3x) plus `--gate-timeout-seconds` for each gate
command. `eta_seconds` is the time until enough
holders reach their declared budgets to free a slot for this position. It is an
estimate and an upper bound: it assumes the budget stays fixed and no
higher-ranked waiter arrives. It is null when too few holders declared a budget.
The daemon `/capacity` response includes `step_queue`, with holders (owner, lane,
`kind` (`step` or `runner_job`), `started_at`, elapsed, budget; no commands or
environment) and ranked waiters
(position, owner, lane, `remote`, wait, ETA). `agent-task capacity` lists those
waiters and marks remote ones. A Cook waiting on runner eligibility reports
`queue_position`, `waiters_ahead`, `queue_scope` (`lab` or `controller`), and
`priority`. The controller computes `eta_seconds` and `estimated_start_at`
from the holder budgets in the Lab's reply, and records the Lab's
`effective_budget`, `active_slots`, and `load_average_1m`. `agent-task status`
and `diagnose` show all of them in the wait message.

`homeboy agent-task capacity` includes Lab observations alongside provider
account capacity. `homeboy runner doctor` includes a `lab_capacity` check.
Observations come from the execution daemon, not the controller's CPU/load.
Disconnected or older daemons without the endpoint cannot supply observations.

## Reserved gate lane

Set `HOMEBOY_LAB_GATE_RESERVE=<n>` in the executing daemon environment to reserve
the last `n` CPU ids in its allowed affinity mask. The count is clamped to leave
at least one CPU for steps. Unset, invalid, or zero values preserve legacy behavior.
Step slices exclude the reserve even without a configured step concurrency limit.

Direct jobs already recorded with kind `runner.exec` use this lane (including
Cook promotion gates); workflow steps remain on step permits. Direct children
are pinned to disjoint shares of the reserved mask. Set
`HOMEBOY_LAB_GATE_SLOTS=<k>` to bound concurrent gates independently of CPU count
and the ordinary runner concurrency limit. It defaults to 1 and is clamped to
1 through the reserve size. Each admitted gate takes the lowest free slot index;
its durable child reservation records that index atomically with admission.
Reservations count before spawn and survive daemon restart. Finishing a gate
frees its slot for reuse. Gates retain the
memory pressure check but ignore host CPU PSI, load, and IO pressure. A full lane
reports `pending_resource_admission` with `pending_admission_reason: gate_slot`.
The daemon `/capacity` response includes `gate_reserve.reserved_cpus`,
`slots`, `active` (entries with `slot`, `cpus`, and `job_id`), and `queued_slots`
(accepted gates not yet holding a child reservation).

A client that waits on a queued job heartbeats it with `POST /jobs/:id/observe`
about once a minute, or opts in at submission with the envelope metadata flag
`queued_owner_heartbeat: true`. If an opted-in job is still queued after
`queued_owner_deadline_ms` (30 minutes) without a heartbeat, the client is gone.
The daemon cancels the job with reason `queued_owner_abandoned`, which frees its
queue position and `queued_slots` entry. `/capacity` reports each opted-in
job's silence in `queued_owner_heartbeat_age_ms`. Detached handoffs and older
clients never opt in, and their queued jobs keep the no-deadline policy
(Refs #18601).

Detached child handles retain wait ownership on early supervisor returns;
reapers wait only their own child PID and do not steal other command statuses.
This does not stop attempt servers or change provider error classification.
Those remain follow-ups to #17603 (Refs #17455).
