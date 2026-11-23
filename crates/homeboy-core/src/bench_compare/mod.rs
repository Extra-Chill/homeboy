//! Base-vs-head corpus comparison for components (`homeboy bench compare`).
//!
//! A component change (for example a Blocks Engine transformer change) is
//! evaluated by importing the same corpus twice — once with the base revision
//! and once with the head revision — and diffing what each side published.
//!
//! Ownership is split deliberately:
//!
//! - **Homeboy core** (this module) owns base/head materialization, the
//!   lifecycle of every process a side starts, the per-route diff, the
//!   regression policy, and the durable artifact.
//! - **The component or its extension** owns how a corpus is imported and
//!   evaluated. The component declares shell commands under
//!   `scripts.bench_compare` in its `homeboy.json`, or a linked extension
//!   declares the [`WORKLOAD_KIND`] workload kind (#15840); the caller may
//!   override both with `--workload-command`. Each command runs once per side inside
//!   that side's checkout and writes a [`SIDE_RESULTS_SCHEMA`] document to
//!   `$HOMEBOY_BENCH_COMPARE_RESULTS`.
//!
//! Every process a side command starts — `php -S` servers, sites, browsers —
//! runs inside a run-owned process containment boundary and is torn down when
//! the command exits, fails, times out, or the compare is cancelled. The
//! artifact asserts that no contained process survived teardown, and a
//! survivor fails the compare regardless of the configured policy.

use serde::{Deserialize, Serialize};
use std::str::FromStr;

mod diff;
mod execute;
mod gate;

pub use diff::{diff_routes, summarize_routes};
pub use execute::{run_bench_compare, BenchCompareRequest, WORKTREE_HEAD};
pub use gate::{
    cook_gate_program, is_cook_gate_command, parse_gate_evidence, BenchCompareGateEvidence,
    COOK_GATE_FLAG, GATE_EVIDENCE_MARKER, GATE_EVIDENCE_SCHEMA,
};

#[cfg(test)]
mod tests;

/// Extension-declared workload kind (`workloads.bench-compare` in an
/// extension manifest) used when the component declares no
/// `scripts.bench_compare`. Its `script` runs once per side.
pub const WORKLOAD_KIND: &str = "bench-compare";
/// Schema of the per-side document a component runner writes.
pub const SIDE_RESULTS_SCHEMA: &str = "homeboy/bench-compare-side/v1";
/// Schema of the durable base-vs-head artifact.
pub const ARTIFACT_SCHEMA: &str = "homeboy/bench-compare/v1";
/// File name of the durable artifact inside the compare's artifact directory.
pub const ARTIFACT_FILE: &str = "bench-compare.json";
/// Data store that holds compare artifacts when no output directory is given.
pub const ARTIFACT_STORE: &str = "bench-compare";
/// Default wall-clock budget for one side's runner commands.
pub const DEFAULT_SIDE_TIMEOUT_SECONDS: u64 = 60 * 60;

/// Environment contract exported to component runner commands.
pub mod env {
    pub const SIDE: &str = "HOMEBOY_BENCH_COMPARE_SIDE";
    pub const REF: &str = "HOMEBOY_BENCH_COMPARE_REF";
    pub const SHA: &str = "HOMEBOY_BENCH_COMPARE_SHA";
    pub const CORPUS: &str = "HOMEBOY_BENCH_COMPARE_CORPUS";
    pub const RESULTS: &str = "HOMEBOY_BENCH_COMPARE_RESULTS";
    pub const ARTIFACT_DIR: &str = "HOMEBOY_BENCH_COMPARE_ARTIFACT_DIR";
    pub const SOURCE: &str = "HOMEBOY_BENCH_COMPARE_SOURCE";
    pub const COMPONENT: &str = "HOMEBOY_BENCH_COMPARE_COMPONENT";
    pub const COMPARE_ID: &str = "HOMEBOY_BENCH_COMPARE_ID";
}

/// When the compare fails, beyond the hard failures (a side runner failing or
/// a contained process surviving teardown) that always fail it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BenchCompareFailOn {
    /// Fail when head introduces an evaluator blocker on any route, or loses a
    /// route base produced.
    #[default]
    NewBlockers,
    /// Fail when any route regresses (new blockers, more findings, or a route
    /// missing on head).
    Regressions,
    /// Report only; never fail on route deltas.
    Never,
}

impl BenchCompareFailOn {
    pub const VALUES: &'static [&'static str] = &["new-blockers", "regressions", "never"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NewBlockers => "new-blockers",
            Self::Regressions => "regressions",
            Self::Never => "never",
        }
    }
}

impl FromStr for BenchCompareFailOn {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "new-blockers" => Ok(Self::NewBlockers),
            "regressions" => Ok(Self::Regressions),
            "never" => Ok(Self::Never),
            other => Err(format!(
                "unknown bench compare policy `{other}`; expected one of: {}",
                Self::VALUES.join(", ")
            )),
        }
    }
}

/// A reusable compare declaration (`homeboy bench compare --spec <file>`).
/// Command-line flags override spec fields. The same file declares a Cook
/// bench gate (`homeboy agent-task cook --verify-bench <file>`), where the
/// Cook owns `base` and `head`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchCompareSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// Component path (repository checkout or a directory inside one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Head revision, or `WORKTREE` for the checkout as it is on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// Corpus path. A value of the form `$NAME` reads the path from the
    /// environment variable `NAME`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus: Option<String>,
    /// Runner commands overriding the component/extension declaration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner: Vec<String>,
    /// Executable names required before either side starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_on: Option<BenchCompareFailOn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

impl BenchCompareSpec {
    pub fn parse(json: &str) -> crate::Result<Self> {
        serde_json::from_str(json).map_err(|error| {
            crate::Error::validation_invalid_argument(
                "bench compare spec",
                format!("invalid bench compare spec: {error}"),
                None,
                Some(vec![format!(
                    "A spec is a JSON object with optional fields: component, path, base, head, corpus, runner (array of shell commands), fail_on ({}), timeout_seconds.",
                    BenchCompareFailOn::VALUES.join(" | ")
                )]),
            )
        })
    }
}

/// One evaluator observation (blocker or finding). Runners may write a bare
/// string or an object; the identity used for diffing is `id`, falling back to
/// `message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BenchCompareFinding {
    Text(String),
    Detailed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        severity: Option<String>,
    },
}

impl BenchCompareFinding {
    /// Stable identity used to decide whether base and head share a finding.
    pub fn key(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Detailed { id, message, .. } => id
                .clone()
                .or_else(|| message.clone())
                .unwrap_or_else(|| "<unnamed>".to_string()),
        }
    }
}

/// What one side published for one corpus route.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCompareRouteObservation {
    pub route: String,
    /// Rendered/published content. Either the raw text or a precomputed
    /// `*_sha256` digest may be supplied for each surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_markup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_markup_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub css: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub css_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<BenchCompareFinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<BenchCompareFinding>,
}

/// The document a runner writes to `$HOMEBOY_BENCH_COMPARE_RESULTS`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCompareSideResults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default)]
    pub routes: Vec<BenchCompareRouteObservation>,
}

/// How one published surface (content, block markup, CSS) changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCompareSurfaceDelta {
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_bytes: Option<usize>,
}

/// How a set of evaluator observations changed from base to head.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCompareFindingDelta {
    pub base_count: usize,
    pub head_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchCompareRouteStatus {
    Improved,
    Regressed,
    Unchanged,
    /// Head produced a route base did not.
    AddedOnHead,
    /// Base produced a route head did not.
    MissingOnHead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCompareRouteDelta {
    pub route: String,
    pub status: BenchCompareRouteStatus,
    /// Whether any published surface differs between base and head.
    pub output_changed: bool,
    pub content: BenchCompareSurfaceDelta,
    pub block_markup: BenchCompareSurfaceDelta,
    pub css: BenchCompareSurfaceDelta,
    pub blockers: BenchCompareFindingDelta,
    pub findings: BenchCompareFindingDelta,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCompareSummary {
    pub routes: usize,
    pub improved: usize,
    pub regressed: usize,
    pub unchanged: usize,
    pub added_on_head: usize,
    pub missing_on_head: usize,
    /// Routes whose published content, markup, or CSS differs.
    pub output_changed: usize,
    pub new_blockers: usize,
    pub resolved_blockers: usize,
    pub new_findings: usize,
    pub resolved_findings: usize,
}

/// Lifecycle evidence for one contained runner command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCompareStepLifecycle {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// `exited`, `timed_out`, `cancelled`, or `spawn_failed`.
    pub termination: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_group_id: Option<u32>,
    /// Whether teardown had to force-kill contained processes.
    pub teardown_forced: bool,
    /// Whether a contained process was still alive after teardown.
    pub leftover_processes: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCompareSideReport {
    /// `base` or `head`.
    pub side: String,
    pub requested_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    /// `git_clone` (a revision in an owned clone) or `worktree`
    /// (an owned snapshot of the checkout, including uncommitted changes).
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dirty: bool,
    pub passed: bool,
    pub route_count: usize,
    pub results_path: String,
    pub artifact_dir: String,
    pub steps: Vec<BenchCompareStepLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Aggregate teardown evidence for the whole compare. `leftover_process_scopes`
/// must be zero for the compare to pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCompareLifecycle {
    pub contained_steps: usize,
    pub forced_teardowns: usize,
    pub leftover_process_scopes: usize,
    pub worktrees_materialized: usize,
    pub worktrees_removed: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchComparePolicyResult {
    pub fail_on: BenchCompareFailOn,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<String>,
}

/// The durable base-vs-head artifact and command payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCompareOutput {
    pub schema: String,
    pub compare_id: String,
    pub component: String,
    pub corpus: String,
    pub started_at: String,
    pub finished_at: String,
    pub base: BenchCompareSideReport,
    pub head: BenchCompareSideReport,
    pub summary: BenchCompareSummary,
    pub policy: BenchComparePolicyResult,
    pub lifecycle: BenchCompareLifecycle,
    pub routes: Vec<BenchCompareRouteDelta>,
    pub artifact_dir: String,
    pub artifact_path: String,
}

/// Apply the regression policy plus the hard lifecycle/runner contract.
pub fn evaluate_policy(
    fail_on: BenchCompareFailOn,
    summary: &BenchCompareSummary,
    base: &BenchCompareSideReport,
    head: &BenchCompareSideReport,
    lifecycle: &BenchCompareLifecycle,
) -> BenchComparePolicyResult {
    let mut violations = Vec::new();
    if lifecycle.cancelled {
        violations.push("compare was cancelled before both sides finished".to_string());
    }
    for side in [base, head] {
        if !side.passed {
            violations.push(format!(
                "{} runner failed: {}",
                side.side,
                side.error.as_deref().unwrap_or("runner did not succeed")
            ));
        }
    }
    if lifecycle.leftover_process_scopes > 0 {
        violations.push(format!(
            "{} contained runner step(s) left processes alive after teardown",
            lifecycle.leftover_process_scopes
        ));
    }
    match fail_on {
        BenchCompareFailOn::NewBlockers => {
            if summary.new_blockers > 0 {
                violations.push(format!(
                    "head introduced {} new evaluator blocker(s)",
                    summary.new_blockers
                ));
            }
            if summary.missing_on_head > 0 {
                violations.push(format!(
                    "{} route(s) produced on base are missing on head",
                    summary.missing_on_head
                ));
            }
        }
        BenchCompareFailOn::Regressions => {
            if summary.regressed > 0 || summary.missing_on_head > 0 {
                violations.push(format!(
                    "{} route(s) regressed and {} route(s) are missing on head",
                    summary.regressed, summary.missing_on_head
                ));
            }
        }
        BenchCompareFailOn::Never => {}
    }
    BenchComparePolicyResult {
        fail_on,
        passed: violations.is_empty(),
        violations,
    }
}
