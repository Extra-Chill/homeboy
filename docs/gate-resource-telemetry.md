# Agent-task gate resource telemetry

Gate reports (including Lab receipts) retain `environment.resources` with
`cpu_user_ms`, `cpu_system_ms`, and `peak_rss_bytes`. CPU is the delta of
`getrusage(RUSAGE_CHILDREN)` across gate execution, including waited-for child
process trees. RSS is the child high-water mark, not summed concurrent tree RSS
and not a delta. Gate-execute runs in a dedicated process; in an in-process
caller the RSS high-water mark can include earlier children, and concurrent
children can contribute to CPU deltas. Unsupported platforms omit resources.

`environment.cargo_target` records `acquire_ms` (including seeding), `seed_ms`,
`seed_bytes`, and nonnegative `growth_bytes` (after minus before). Sizing excludes
Homeboy lifecycle sidecars. `seeded` means the target started with bytes, **not**
that Cargo reused fingerprints or avoided compilation; `empty` means zero bytes.
These replace the misleading `hit`/`miss` labels. Acquisition and seed timings
overlap and must not be added together. Host pressure snapshots retain Linux
`/proc/loadavg` and CPU/memory/IO PSI at start and end; unavailable files are null.

After dispatch the controller persists a small `agent_task_gate_resources`
artifact with the receipt metrics as JSON and metadata. It contains no command
output or Lab filesystem paths. Thus Lab Cooks need no remote artifact download
to contribute to `homeboy runs hotspots --kind agent-task`. Historical gates
without CPU measurements are omitted rather than counted as zero; growth has a
separate sample count because gates need not acquire a Cargo target.

## Example and #18339 comparison

Illustrative output (synthetic values, not a measured performance claim):

```text
Cook wall time (submission-to-PR ms): count=1 p50=1500 p95=1500 mean=1500
  gate.cargo-test: samples=1 CPU total=48ms p50=48.0ms peak RSS=4096B growth=50B (samples=1)
```

Before deploying #18339, record a telemetry-enabled Cook cohort with:

```sh
homeboy runs hotspots --kind agent-task --component homeboy --homeboy-version BEFORE --last 100 --json > before.json
```

After deploying #18339, run comparable Cooks (same gates, candidate workload and
host capacity), substituting `AFTER` and `after.json`. Compare `gate_resources`
rows by `phase`: CPU total divided by count and p50, peak RSS, and growth total
divided by growth sample count. Match cohort sizes and inspect host pressure and
seed/acquire evidence before attributing differences to compile-once behavior.
Use the existing `--baseline-version BEFORE --candidate-version AFTER` phase
comparison for wall-time context; its regression verdict remains wall-time based,
not a CPU/growth threshold. No workspace or target-sharing behavior changes here.
