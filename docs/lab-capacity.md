# Lab capacity admission

Lab execution samples `available_parallelism` and the first `/proc/loadavg`
value before admission. The policy is `min(limit, max(1, floor(cpus - load)))`:
load can lower the configured runner concurrency limit, never raise it. A
single slot remains available for forward progress on a saturated host. Missing
or invalid load data falls back to the CPU bound. Existing work is not killed
when load rises.

Direct runner executions retain their durable runner-capacity reservation and
FIFO queue. Detached Cook execution steps also wait before launching heavy
provider/gate work; launcher steps do not hold a slot while waiting for a nested
execution step. Waiting progress reports `waiting_for: lab_capacity`,
`queue_position`, `load_average_1m`, and `effective_budget`. `eta_seconds` is null:
load and provider durations do not support an honest completion estimate.

`homeboy agent-task capacity` includes Lab observations alongside provider
account capacity. `homeboy runner doctor` includes a `lab_capacity` check.
Observations come from the execution daemon, not the controller's CPU/load.
Disconnected or older daemons without the endpoint cannot supply observations.

Detached child handles retain wait ownership on early supervisor returns;
reapers wait only their own child PID and do not steal other command statuses.
This does not stop attempt servers or change provider error classification.
Those remain follow-ups to #17603 (Refs #17455).
