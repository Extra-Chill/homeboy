# Lab capacity admission

Lab execution samples `available_parallelism` and the first `/proc/loadavg`
value before admission. The policy is `min(limit, max(1, floor(cpus - load)))`:
load can lower the configured runner concurrency limit, never raise it. A
single slot remains available for forward progress on a saturated host. Missing
or invalid load data falls back to the CPU bound. Existing work is not killed
when load rises.

Heavy execution and workflow steps take a slot before launching. Waiting owners
with the fewest active slots go first, with arrival order breaking ties. A waiting
step polls actively and releases its ticket on cancellation. Launcher steps do
not hold a slot while waiting for children. By default, direct runner job
reservations keep their configured concurrency limit unchanged. Waiting progress reports `waiting_for: lab_capacity`,
`queue_position`, `load_average_1m`, and `effective_budget`. `eta_seconds` is null:
load and provider durations do not support an honest completion estimate.

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
