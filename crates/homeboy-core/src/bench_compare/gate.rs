//! `homeboy bench compare` attached to a Cook as a deterministic gate.
//!
//! A Cook declares a bench gate with `--verify-bench <spec>`. The controller
//! compiles the [`BenchCompareSpec`] into one visible verify program,
//! [`cook_gate_program`], whose base is the Cook's immutable base
//! (`__HOMEBOY_COOK_BASE_SHA__`, bound once the candidate is harvested) and
//! whose head is the candidate checkout the gate runs in ([`WORKTREE_HEAD`]).
//! The spec's `fail_on` is the declared regression policy; the gate passes or
//! fails with the compare's policy verdict.
//!
//! The compare already runs both sides itself, so a bench gate is a
//! repro-style comparison: Cook never replays it against the base to explain a
//! failure ([`is_cook_gate_command`]). A base without the feature is one side
//! of the comparison, never an inherited failure.
//!
//! When it finishes, the gate writes one [`GATE_EVIDENCE_MARKER`] line to
//! stderr carrying [`BenchCompareGateEvidence`]: the policy verdict, the
//! aggregate summary, and the durable artifact path. Gate execution parses it
//! back with [`parse_gate_evidence`] so the Cook result, summary, and
//! notification can link the artifact.

use serde::{Deserialize, Serialize};
use std::path::Path;

use super::{
    BenchCompareFailOn, BenchCompareOutput, BenchCompareSpec, BenchCompareSummary, WORKTREE_HEAD,
};

/// Hidden `bench compare` flag that marks an invocation as a Cook gate. It
/// also identifies bench gates in a Cook's declared gate programs.
pub const COOK_GATE_FLAG: &str = "--cook-gate";
/// Prefix of the single stderr line a Cook bench gate emits on completion.
pub const GATE_EVIDENCE_MARKER: &str = "HOMEBOY_BENCH_COMPARE_GATE ";
/// Schema of [`BenchCompareGateEvidence`].
pub const GATE_EVIDENCE_SCHEMA: &str = "homeboy/bench-compare-gate/v1";
/// Placeholder Cook binds to its immutable base before running gates.
const COOK_BASE_PLACEHOLDER: &str = "__HOMEBOY_COOK_BASE_SHA__";

/// What a Cook records about one bench gate run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCompareGateEvidence {
    pub schema: String,
    pub compare_id: String,
    pub passed: bool,
    pub fail_on: BenchCompareFailOn,
    pub summary: BenchCompareSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<String>,
    /// Durable `bench-compare.json` artifact on the host that ran the gate.
    pub artifact_path: String,
}

impl From<&BenchCompareOutput> for BenchCompareGateEvidence {
    fn from(output: &BenchCompareOutput) -> Self {
        Self {
            schema: GATE_EVIDENCE_SCHEMA.to_string(),
            compare_id: output.compare_id.clone(),
            passed: output.policy.passed,
            fail_on: output.policy.fail_on,
            summary: output.summary.clone(),
            violations: output.policy.violations.clone(),
            artifact_path: output.artifact_path.clone(),
        }
    }
}

impl BenchCompareGateEvidence {
    /// The single stderr line a Cook bench gate emits.
    pub fn marker_line(&self) -> String {
        format!(
            "{GATE_EVIDENCE_MARKER}{}",
            serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
        )
    }

    /// One reviewer-facing line: verdict, policy, aggregate deltas.
    pub fn headline(&self) -> String {
        format!(
            "{} ({}): {} route(s), {} regressed, {} improved, {} new blocker(s), {} missing on head",
            if self.passed { "passed" } else { "failed" },
            self.fail_on.as_str(),
            self.summary.routes,
            self.summary.regressed,
            self.summary.improved,
            self.summary.new_blockers,
            self.summary.missing_on_head,
        )
    }
}

/// The last bench gate evidence line in `text`, if any.
pub fn parse_gate_evidence(text: &str) -> Option<BenchCompareGateEvidence> {
    text.lines()
        .rev()
        .filter_map(|line| line.trim_start().strip_prefix(GATE_EVIDENCE_MARKER))
        .find_map(|json| serde_json::from_str::<BenchCompareGateEvidence>(json.trim()).ok())
        .filter(|evidence| evidence.schema == GATE_EVIDENCE_SCHEMA)
}

/// Whether a declared gate program is a Cook bench gate.
pub fn is_cook_gate_command(command: &str) -> bool {
    command.contains("bench compare") && command.contains(COOK_GATE_FLAG)
}

/// Compile a bench compare spec into the Cook gate program. Cook owns both
/// revisions, so a spec that pins `base` or `head` is rejected, as is an
/// absolute `path` (the gate runs inside the candidate checkout).
pub fn cook_gate_program(spec: &BenchCompareSpec) -> crate::Result<String> {
    let invalid = |problem: String| {
        crate::Error::validation_invalid_argument(
            "--verify-bench",
            problem,
            None,
            Some(vec![
                "A Cook bench gate compares the Cook's base with its candidate; declare component, path (relative), corpus, runner, fail_on, and timeout_seconds only.".to_string(),
            ]),
        )
    };
    if spec.base.is_some() || spec.head.is_some() {
        return Err(invalid(
            "a Cook bench gate spec must not set `base` or `head`: the Cook's immutable base and its candidate are the two sides".to_string(),
        ));
    }
    let path = spec.path.as_deref().unwrap_or(".");
    if Path::new(path).is_absolute()
        || Path::new(path)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(invalid(format!(
            "a Cook bench gate spec `path` must stay within the candidate checkout, got `{path}`"
        )));
    }
    if spec
        .corpus
        .as_deref()
        .is_none_or(|corpus| corpus.trim().is_empty())
    {
        return Err(invalid(
            "a Cook bench gate spec needs a `corpus` (a path, or `$NAME` naming a gate environment variable)".to_string(),
        ));
    }

    let quote = homeboy_engine_primitives::shell::shell_arg;
    let mut program = vec![
        "homeboy".to_string(),
        "bench".to_string(),
        "compare".to_string(),
        COOK_GATE_FLAG.to_string(),
        "--base".to_string(),
        COOK_BASE_PLACEHOLDER.to_string(),
        "--head".to_string(),
        WORKTREE_HEAD.to_string(),
        "--path".to_string(),
        quote(path),
    ];
    if let Some(component) = &spec.component {
        program.extend(["--component".to_string(), quote(component)]);
    }
    if let Some(corpus) = &spec.corpus {
        program.extend(["--corpus".to_string(), quote(corpus)]);
    }
    for tool in &spec.required_tools {
        program.extend(["--required-tool".to_string(), quote(tool)]);
    }
    for command in &spec.runner {
        program.extend(["--workload-command".to_string(), quote(command)]);
    }
    program.extend([
        "--fail-on".to_string(),
        spec.fail_on.unwrap_or_default().as_str().to_string(),
    ]);
    if let Some(timeout) = spec.timeout_seconds {
        program.extend(["--timeout-seconds".to_string(), timeout.to_string()]);
    }
    Ok(program.join(" "))
}
