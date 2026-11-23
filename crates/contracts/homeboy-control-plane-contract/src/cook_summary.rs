//! Versioned caller-facing Cook summary (#18817).
//!
//! Every Cook read surface — `agent-task status`, `status --watch`,
//! `diagnose`, the launch output and the terminal notification payload —
//! emits this object under the same top-level key, [`COOK_SUMMARY_KEY`], so a
//! caller reads `.cook_summary.status` identically everywhere instead of
//! grepping surface-specific payloads.

use serde::{Deserialize, Serialize};

pub const COOK_SUMMARY_SCHEMA: &str = "homeboy/cook-summary/v1";

/// The top-level key every Cook read surface carries the summary under.
pub const COOK_SUMMARY_KEY: &str = "cook_summary";

/// Bound on the failing test ids one gate reports in the summary.
pub const COOK_SUMMARY_FAILING_TEST_LIMIT: usize = 20;

/// Bound on the gates the summary reports.
pub const COOK_SUMMARY_GATE_LIMIT: usize = 32;

/// Bound on the characters of `why_no_pr`.
pub const COOK_SUMMARY_REASON_MAX_CHARS: usize = 512;

/// Bound on the serialized summary, so every surface can carry it whole
/// inside its own output budget.
pub const COOK_SUMMARY_MAX_BYTES: usize = 4 * 1024;

/// One deterministic gate's verdict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CookSummaryGate {
    pub name: String,
    pub state: String,
    pub failing_tests: Vec<String>,
    /// Present only on a bench compare gate (`--verify-bench`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bench_compare: Option<CookSummaryBenchCompare>,
}

/// A bench compare gate's policy verdict, aggregate deltas, and artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CookSummaryBenchCompare {
    pub passed: bool,
    /// The declared regression policy (`new-blockers`, `regressions`, `never`).
    pub fail_on: String,
    pub routes: usize,
    pub regressed: usize,
    pub improved: usize,
    pub new_blockers: usize,
    pub missing_on_head: usize,
    /// Durable compare artifact on the host that ran the gate.
    pub artifact_path: String,
}

/// The stable summary of one Cook run. Every field is always serialized; an
/// unknown value is `null`, never absent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CookSummary {
    pub schema: String,
    /// The Cook status label (for example `review_ready`, `gate_failed`,
    /// `candidate_recoverable`, `running`).
    pub status: String,
    /// The Cook run lifecycle phase (`queued`, `running`, `continuing`,
    /// `candidate_recoverable`, `succeeded`, `partial_failure`, `failed`,
    /// `cancelled`, `unknown`).
    pub phase: String,
    /// The run's execution ended. A `continuing` run is terminal but still
    /// carried by its continuation owner.
    pub terminal: bool,
    /// The candidate (or published) commit.
    pub head_sha: Option<String>,
    /// The base the candidate was built and verified on.
    pub base_sha: Option<String>,
    pub gates: Vec<CookSummaryGate>,
    pub pr_url: Option<String>,
    /// Why no pull request exists, as `<status>: <reason>`. Non-null whenever
    /// `pr_url` is null on a terminal run; null whenever `pr_url` is set.
    pub why_no_pr: Option<String>,
}

impl CookSummary {
    /// A summary for a surface that has no durable run record to project
    /// (a recipe-only Cook, a CI receipt, a report whose record is gone).
    pub fn unobserved(
        status: impl Into<String>,
        terminal: bool,
        pr_url: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        let status = status.into();
        let why_no_pr =
            (pr_url.is_none() && terminal).then(|| format!("{status}: {}", reason.into()));
        Self {
            schema: COOK_SUMMARY_SCHEMA.to_string(),
            status,
            phase: "unknown".to_string(),
            terminal,
            head_sha: None,
            base_sha: None,
            gates: Vec::new(),
            pr_url,
            why_no_pr,
        }
        .bounded()
    }

    /// Fit the summary within its bounds: truncate `why_no_pr`, then drop
    /// failing test ids (largest list first), then trailing gates.
    pub fn bounded(mut self) -> Self {
        if let Some(reason) = &self.why_no_pr {
            if reason.chars().count() > COOK_SUMMARY_REASON_MAX_CHARS {
                let kept: String = reason
                    .chars()
                    .take(COOK_SUMMARY_REASON_MAX_CHARS - 3)
                    .collect();
                self.why_no_pr = Some(format!("{kept}..."));
            }
        }
        self.gates.truncate(COOK_SUMMARY_GATE_LIMIT);
        for gate in &mut self.gates {
            gate.failing_tests.truncate(COOK_SUMMARY_FAILING_TEST_LIMIT);
        }
        while self.serialized_len() > COOK_SUMMARY_MAX_BYTES {
            if let Some(gate) = self
                .gates
                .iter_mut()
                .filter(|gate| !gate.failing_tests.is_empty())
                .max_by_key(|gate| gate.failing_tests.len())
            {
                gate.failing_tests.pop();
            } else if self.gates.pop().is_none() {
                break;
            }
        }
        self
    }

    fn serialized_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |bytes| bytes.len())
    }

    /// Check the contract invariants every surface must uphold.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != COOK_SUMMARY_SCHEMA {
            return Err(format!("unexpected cook summary schema {}", self.schema));
        }
        if self.status.trim().is_empty() || self.phase.trim().is_empty() {
            return Err("cook summary status and phase must be non-empty".to_string());
        }
        if self.terminal && self.pr_url.is_none() && self.why_no_pr.is_none() {
            return Err("a terminal cook summary without pr_url must name why_no_pr".to_string());
        }
        if self.pr_url.is_some() && self.why_no_pr.is_some() {
            return Err("a cook summary with pr_url must not name why_no_pr".to_string());
        }
        if self.gates.len() > COOK_SUMMARY_GATE_LIMIT
            || self
                .gates
                .iter()
                .any(|gate| gate.failing_tests.len() > COOK_SUMMARY_FAILING_TEST_LIMIT)
        {
            return Err("cook summary exceeds its gate bounds".to_string());
        }
        if self
            .why_no_pr
            .as_ref()
            .is_some_and(|reason| reason.chars().count() > COOK_SUMMARY_REASON_MAX_CHARS)
            || self.serialized_len() > COOK_SUMMARY_MAX_BYTES
        {
            return Err("cook summary exceeds its size bounds".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_serializes_every_field_and_round_trips() {
        let summary = CookSummary {
            schema: COOK_SUMMARY_SCHEMA.to_string(),
            status: "candidate_recoverable".to_string(),
            phase: "candidate_recoverable".to_string(),
            terminal: true,
            head_sha: Some("abc".to_string()),
            base_sha: None,
            gates: vec![CookSummaryGate {
                name: "cargo test".to_string(),
                state: "failed".to_string(),
                failing_tests: vec!["a::b".to_string()],
                bench_compare: None,
            }],
            pr_url: None,
            why_no_pr: Some("candidate_recoverable: publication not attempted".to_string()),
        };
        summary.validate().expect("valid");
        let value = serde_json::to_value(&summary).expect("serialize");
        for key in [
            "schema",
            "status",
            "phase",
            "terminal",
            "head_sha",
            "base_sha",
            "gates",
            "pr_url",
            "why_no_pr",
        ] {
            assert!(value.get(key).is_some(), "{key} is always present");
        }
        assert_eq!(
            serde_json::from_value::<CookSummary>(value).expect("deserialize"),
            summary
        );
    }

    #[test]
    fn terminal_summary_without_pr_requires_a_reason() {
        let mut summary = CookSummary::unobserved("failed", true, None, "run ended");
        assert_eq!(summary.why_no_pr.as_deref(), Some("failed: run ended"));
        summary.validate().expect("valid");
        summary.why_no_pr = None;
        assert!(summary.validate().is_err());

        let long = CookSummary::unobserved("failed", true, None, "x".repeat(10_000));
        long.validate().expect("long reasons are bounded");

        let mut many = CookSummary::unobserved("gate_failed", true, None, "gates failed");
        many.gates = (0..COOK_SUMMARY_GATE_LIMIT)
            .map(|index| CookSummaryGate {
                name: format!("gate-{index}"),
                state: "failed".to_string(),
                failing_tests: (0..COOK_SUMMARY_FAILING_TEST_LIMIT)
                    .map(|test| format!("suite::module::case_{test}_{}", "y".repeat(40)))
                    .collect(),
                bench_compare: None,
            })
            .collect();
        assert!(many.validate().is_err());
        let many = many.bounded();
        many.validate().expect("bounded summaries fit");

        let running = CookSummary::unobserved("running", false, None, "unused");
        assert_eq!(running.why_no_pr, None);
        running.validate().expect("in-flight runs need no reason");

        let published = CookSummary::unobserved(
            "review_ready",
            true,
            Some("https://github.com/o/r/pull/1".to_string()),
            "unused",
        );
        assert_eq!(published.why_no_pr, None);
        published.validate().expect("valid");
    }
}
