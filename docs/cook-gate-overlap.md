# Cook gate overlap and timing

Cook warns at admission when visible gate declarations include both a
`review lint` command and a direct `cargo fmt` or `cargo check` command. The
warning is printed and persisted as `cook_gate_overlap_warning` run metadata.
Private commands are not inspected for this public warning.

The policy is **warn, not dedupe**. Rust lint extensions commonly run fmt and
clippy, but extension versions, package selection, target selection and
`--locked` constraints can differ. Every declared command remains part of the
verification contract. Compare the extension's actual checks with the declared
Cargo command before removing an overlapping gate from a recipe.

Local gate timing records admission under `stage.local_gate_admission` and
execution under `gate.<command>`. `roundtrip.gate.<command>` measures the whole
controller call, including placement, queueing and retries. For local execution
cost use the `gate.*` spans, not roundtrip spans. Remote roundtrip spans likewise
must not be interpreted as process execution cost; remote process telemetry
belongs to the runner's gate report.

Managed Cargo targets already retain candidate-keyed targets and evict inactive
stores through shared Cargo target retention. This does not by itself guarantee
workspace artifact reuse across different absolute source paths. A stable source
path or a measured compiler-cache deployment is still required to establish the
Lab performance target in issue #19191.
