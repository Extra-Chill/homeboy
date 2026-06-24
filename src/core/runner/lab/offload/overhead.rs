//! Runner-agnostic Lab offload overhead accounting (#3001).
//!
//! Lab benchmarks and hot-command runs need to distinguish time spent in
//! runner *setup* (selecting a runner, connecting/preflighting it, syncing the
//! workspace, importing artifacts) from time spent in the *workload* itself
//! (the remote command). Without this split a benchmark can accidentally
//! measure sync/connect churn instead of the command under test.
//!
//! [`LabOffloadOverhead`] records a per-phase `Duration` map plus a total
//! `lab_overhead_ms`. It is deliberately runner-agnostic: the same phase model
//! applies across local-fallback, SSH, daemon, and reverse-tunnel runners. The
//! `remote_exec` phase captures the *workload* command duration separately so
//! reports can subtract it from `lab_overhead_ms` — overhead is the sum of the
//! setup phases only and never folds in workload time.

use std::time::{Duration, Instant};

/// The distinct phases of a Lab offload run that we time independently.
///
/// All variants except [`LabOffloadPhase::RemoteExec`] are *overhead* (runner
/// setup). `RemoteExec` is the *workload* and is tracked separately so reports
/// can separate `lab_overhead_ms` from workload command duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum LabOffloadPhase {
    /// Choosing which runner (or local fallback) will run the command.
    Selection,
    /// Connecting to / preflighting the chosen runner (capability, daemon
    /// health, version-skew checks).
    Preflight,
    /// Synchronizing the workspace/source checkout to the runner.
    WorkspaceSync,
    /// Executing the workload command on the runner. This is the WORKLOAD, not
    /// overhead; tracked separately so it stays subtractable from the total.
    RemoteExec,
    /// Parsing the remote command output/streams.
    OutputParse,
    /// Importing artifacts / structured-output files back from the runner.
    ArtifactImport,
}

impl LabOffloadPhase {
    /// Stable, runner-agnostic key used in serialized metadata.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            LabOffloadPhase::Selection => "selection",
            LabOffloadPhase::Preflight => "preflight",
            LabOffloadPhase::WorkspaceSync => "workspace_sync",
            LabOffloadPhase::RemoteExec => "remote_exec",
            LabOffloadPhase::OutputParse => "output_parse",
            LabOffloadPhase::ArtifactImport => "artifact_import",
        }
    }

    /// The setup phases in canonical order, used to render a stable per-phase
    /// duration map even when some phases were never reached.
    pub(crate) const fn overhead_phases() -> [LabOffloadPhase; 5] {
        [
            LabOffloadPhase::Selection,
            LabOffloadPhase::Preflight,
            LabOffloadPhase::WorkspaceSync,
            LabOffloadPhase::OutputParse,
            LabOffloadPhase::ArtifactImport,
        ]
    }
}

/// Records which runner the offload attempted to select before it either
/// proceeded or fell back to local execution. Surfaced consistently for both
/// automatic (default-runner) and explicit (`--runner`) selection so a
/// fallback-to-local command always reports what it tried first.
#[derive(Debug, Clone, Default)]
pub(crate) struct AttemptedSelection {
    /// Runner id the offload attempted to use, if one was selected.
    pub runner_id: Option<String>,
    /// How the runner was selected (`explicit`, `default`, …).
    pub source: Option<String>,
    /// Runner transport mode (`ssh`, `daemon`, `reverse_tunnel`, …).
    pub mode: Option<String>,
}

impl AttemptedSelection {
    fn is_empty(&self) -> bool {
        self.runner_id.is_none() && self.source.is_none() && self.mode.is_none()
    }

    fn to_metadata(&self) -> Option<serde_json::Value> {
        if self.is_empty() {
            return None;
        }
        Some(serde_json::json!({
            "runner_id": self.runner_id,
            "source": self.source,
            "mode": self.mode,
        }))
    }
}

/// Accumulates per-phase Lab offload timings and the fallback reason / attempted
/// selection so they can be attached to the run metadata.
///
/// Construct with [`LabOffloadOverhead::start`], wrap each phase with
/// [`LabOffloadOverhead::phase`] (RAII timer) or
/// [`LabOffloadOverhead::record`], and serialize with
/// [`LabOffloadOverhead::to_metadata`].
#[derive(Debug, Clone, Default)]
pub(crate) struct LabOffloadOverhead {
    selection: Option<Duration>,
    preflight: Option<Duration>,
    workspace_sync: Option<Duration>,
    remote_exec: Option<Duration>,
    output_parse: Option<Duration>,
    artifact_import: Option<Duration>,
    attempted: AttemptedSelection,
    fallback_reason: Option<String>,
}

impl LabOffloadOverhead {
    /// Begin overhead accounting for a fresh offload attempt.
    pub(crate) fn start() -> Self {
        Self::default()
    }

    fn slot(&mut self, phase: LabOffloadPhase) -> &mut Option<Duration> {
        match phase {
            LabOffloadPhase::Selection => &mut self.selection,
            LabOffloadPhase::Preflight => &mut self.preflight,
            LabOffloadPhase::WorkspaceSync => &mut self.workspace_sync,
            LabOffloadPhase::RemoteExec => &mut self.remote_exec,
            LabOffloadPhase::OutputParse => &mut self.output_parse,
            LabOffloadPhase::ArtifactImport => &mut self.artifact_import,
        }
    }

    /// Add `elapsed` to a phase. Repeated records for the same phase accumulate
    /// (e.g. multiple preflight checks) rather than overwrite.
    pub(crate) fn record(&mut self, phase: LabOffloadPhase, elapsed: Duration) {
        let slot = self.slot(phase);
        *slot = Some(slot.unwrap_or_default() + elapsed);
    }

    /// Start a RAII timer for `phase`; the elapsed time is recorded when the
    /// returned guard is dropped (or via [`PhaseTimer::finish`]).
    pub(crate) fn phase(&mut self, phase: LabOffloadPhase) -> PhaseTimer<'_> {
        PhaseTimer {
            overhead: self,
            phase,
            started: Instant::now(),
            done: false,
        }
    }

    /// Record the runner the offload attempted to select. Always called once a
    /// selection is resolved, before connect/preflight, so a later fallback
    /// still reports what was attempted.
    pub(crate) fn set_attempted(&mut self, runner_id: &str, source: &str, mode: Option<&str>) {
        self.attempted = AttemptedSelection {
            runner_id: Some(runner_id.to_string()),
            source: Some(source.to_string()),
            mode: mode.map(str::to_string),
        };
    }

    /// Record why the offload fell back to local execution (or was skipped).
    pub(crate) fn set_fallback_reason(&mut self, reason: &str) {
        self.fallback_reason = Some(reason.to_string());
    }

    /// Sum of the setup phases only (workload `remote_exec` excluded).
    pub(crate) fn overhead_total(&self) -> Duration {
        LabOffloadPhase::overhead_phases()
            .into_iter()
            .filter_map(|phase| *self.peek(phase))
            .sum()
    }

    fn peek(&self, phase: LabOffloadPhase) -> &Option<Duration> {
        match phase {
            LabOffloadPhase::Selection => &self.selection,
            LabOffloadPhase::Preflight => &self.preflight,
            LabOffloadPhase::WorkspaceSync => &self.workspace_sync,
            LabOffloadPhase::RemoteExec => &self.remote_exec,
            LabOffloadPhase::OutputParse => &self.output_parse,
            LabOffloadPhase::ArtifactImport => &self.artifact_import,
        }
    }

    fn millis(duration: Duration) -> u64 {
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    }

    /// Serialize to a runner-agnostic metadata object:
    ///
    /// ```json
    /// {
    ///   "schema": "homeboy/lab-offload-overhead/v1",
    ///   "phase_durations_ms": { "selection": 1, "preflight": 4, ... },
    ///   "lab_overhead_ms": 12,
    ///   "workload_ms": 980,
    ///   "fallback_reason": null,
    ///   "attempted_selection": { "runner_id": "...", "source": "...", "mode": "..." }
    /// }
    /// ```
    pub(crate) fn to_metadata(&self) -> serde_json::Value {
        let mut phase_durations = serde_json::Map::new();
        for phase in LabOffloadPhase::overhead_phases() {
            if let Some(duration) = self.peek(phase) {
                phase_durations.insert(
                    phase.key().to_string(),
                    serde_json::json!(Self::millis(*duration)),
                );
            }
        }
        let workload_ms = self.remote_exec.map(Self::millis);
        serde_json::json!({
            "schema": "homeboy/lab-offload-overhead/v1",
            "phase_durations_ms": phase_durations,
            "lab_overhead_ms": Self::millis(self.overhead_total()),
            "workload_ms": workload_ms,
            "fallback_reason": self.fallback_reason,
            "attempted_selection": self.attempted.to_metadata(),
        })
    }
}

/// Attach the serialized overhead object to an existing Lab offload metadata
/// value under the `lab_offload_overhead` key, so reports and artifacts can
/// read per-phase setup timings and the total `lab_overhead_ms` alongside the
/// rest of the offload metadata. No-op-safe on any JSON object.
pub(crate) fn attach_lab_offload_overhead(
    metadata: &mut serde_json::Value,
    overhead: &LabOffloadOverhead,
) {
    if let Some(map) = metadata.as_object_mut() {
        map.insert("lab_offload_overhead".to_string(), overhead.to_metadata());
    }
}

/// RAII timer that records its elapsed time into a [`LabOffloadOverhead`] phase
/// when dropped. Lets a phase be wrapped without manual `Instant` bookkeeping.
pub(crate) struct PhaseTimer<'a> {
    overhead: &'a mut LabOffloadOverhead,
    phase: LabOffloadPhase,
    started: Instant,
    done: bool,
}

impl PhaseTimer<'_> {
    /// Stop timing now and record the elapsed duration.
    pub(crate) fn finish(mut self) {
        self.flush();
    }

    fn flush(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        let elapsed = self.started.elapsed();
        self.overhead.record(self.phase, elapsed);
    }
}

impl Drop for PhaseTimer<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overhead_total_excludes_workload_and_sums_setup_phases() {
        let mut overhead = LabOffloadOverhead::start();
        overhead.record(LabOffloadPhase::Selection, Duration::from_millis(5));
        overhead.record(LabOffloadPhase::Preflight, Duration::from_millis(10));
        overhead.record(LabOffloadPhase::WorkspaceSync, Duration::from_millis(20));
        overhead.record(LabOffloadPhase::OutputParse, Duration::from_millis(3));
        overhead.record(LabOffloadPhase::ArtifactImport, Duration::from_millis(2));
        // Workload is NOT overhead and must not inflate the total.
        overhead.record(LabOffloadPhase::RemoteExec, Duration::from_millis(1000));

        assert_eq!(overhead.overhead_total(), Duration::from_millis(40));

        let metadata = overhead.to_metadata();
        assert_eq!(metadata["lab_overhead_ms"], serde_json::json!(40));
        assert_eq!(metadata["workload_ms"], serde_json::json!(1000));
        assert_eq!(
            metadata["schema"],
            serde_json::json!("homeboy/lab-offload-overhead/v1")
        );
        let durations = metadata["phase_durations_ms"].as_object().unwrap();
        assert_eq!(durations["selection"], serde_json::json!(5));
        assert_eq!(durations["preflight"], serde_json::json!(10));
        assert_eq!(durations["workspace_sync"], serde_json::json!(20));
        assert_eq!(durations["output_parse"], serde_json::json!(3));
        assert_eq!(durations["artifact_import"], serde_json::json!(2));
        // The workload phase is not part of the overhead duration map.
        assert!(!durations.contains_key("remote_exec"));
    }

    #[test]
    fn records_per_phase_overhead_and_total_for_offloaded_run() {
        let mut overhead = LabOffloadOverhead::start();
        overhead.set_attempted("lab-ssh", "default", Some("ssh"));
        {
            let _selection = overhead.phase(LabOffloadPhase::Selection);
        }
        overhead.record(LabOffloadPhase::Preflight, Duration::from_millis(7));
        overhead.record(LabOffloadPhase::WorkspaceSync, Duration::from_millis(11));
        overhead.record(LabOffloadPhase::RemoteExec, Duration::from_millis(500));
        overhead.record(LabOffloadPhase::OutputParse, Duration::from_millis(1));

        let metadata = overhead.to_metadata();
        // Per-phase map present for the setup phases that ran.
        let durations = metadata["phase_durations_ms"].as_object().unwrap();
        assert!(durations.contains_key("selection"));
        assert!(durations.contains_key("preflight"));
        assert!(durations.contains_key("workspace_sync"));
        assert!(durations.contains_key("output_parse"));
        // Total overhead present and workload separable.
        assert!(metadata["lab_overhead_ms"].as_u64().is_some());
        assert_eq!(metadata["workload_ms"], serde_json::json!(500));
        // Fallback reason absent for a successful offload.
        assert!(metadata["fallback_reason"].is_null());
        // Attempted selection recorded.
        let attempted = &metadata["attempted_selection"];
        assert_eq!(attempted["runner_id"], serde_json::json!("lab-ssh"));
        assert_eq!(attempted["source"], serde_json::json!("default"));
        assert_eq!(attempted["mode"], serde_json::json!("ssh"));
    }

    #[test]
    fn records_attempted_selection_and_reason_on_local_fallback() {
        let mut overhead = LabOffloadOverhead::start();
        // The offload attempted a default runner, ran selection + preflight,
        // then fell back to local execution.
        overhead.set_attempted("lab-explicit", "explicit", Some("daemon"));
        overhead.record(LabOffloadPhase::Selection, Duration::from_millis(2));
        overhead.record(LabOffloadPhase::Preflight, Duration::from_millis(9));
        overhead.set_fallback_reason("runner capability preflight failed: missing toolchain");

        let metadata = overhead.to_metadata();
        assert_eq!(
            metadata["fallback_reason"],
            serde_json::json!("runner capability preflight failed: missing toolchain")
        );
        let attempted = &metadata["attempted_selection"];
        assert_eq!(attempted["runner_id"], serde_json::json!("lab-explicit"));
        assert_eq!(attempted["source"], serde_json::json!("explicit"));
        assert_eq!(attempted["mode"], serde_json::json!("daemon"));
        // The attempted selection/preflight overhead is still recorded even
        // though the command ultimately ran locally.
        let durations = metadata["phase_durations_ms"].as_object().unwrap();
        assert_eq!(durations["selection"], serde_json::json!(2));
        assert_eq!(durations["preflight"], serde_json::json!(9));
        // No remote exec happened, so workload is absent.
        assert!(metadata["workload_ms"].is_null());
        assert_eq!(metadata["lab_overhead_ms"], serde_json::json!(11));
    }

    #[test]
    fn attempted_selection_absent_when_no_runner_was_chosen() {
        let mut overhead = LabOffloadOverhead::start();
        overhead.set_fallback_reason("no_default_runner");
        let metadata = overhead.to_metadata();
        assert!(metadata["attempted_selection"].is_null());
        assert_eq!(
            metadata["fallback_reason"],
            serde_json::json!("no_default_runner")
        );
    }
}
