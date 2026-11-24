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

/// Bound on the attempt lines the summary reports (the latest are kept).
pub const COOK_SUMMARY_ATTEMPT_LIMIT: usize = 8;

/// Bound on the characters of one attempt line.
pub const COOK_SUMMARY_ATTEMPT_MAX_CHARS: usize = 256;

/// Bound on the characters of `why_no_pr`.
pub const COOK_SUMMARY_REASON_MAX_CHARS: usize = 512;

/// Bound on the task items the summary's delivery accounting reports.
pub const COOK_SUMMARY_DELIVERY_LIMIT: usize = 16;

/// Bound on the characters of one delivery item's text and of its reason.
pub const COOK_SUMMARY_DELIVERY_TEXT_MAX_CHARS: usize = 160;

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

/// Whether the agent delivered one item of the task's checklist (#18884).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CookDeliveryItemStatus {
    Delivered,
    NotDelivered,
}

/// One item of the task's checklist and the agent's delivery verdict on it.
/// A `not_delivered` item carries the agent's reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CookDeliveryItem {
    pub item: String,
    pub status: CookDeliveryItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CookDeliveryItem {
    pub fn is_delivered(&self) -> bool {
        self.status == CookDeliveryItemStatus::Delivered
    }
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
    /// One line per Cook attempt, oldest first:
    /// `attempt N: <candidate sha> · gates <verdicts> · <disposition>`, where
    /// disposition is `published`, `superseded`, `discarded` or the latest
    /// attempt's pending state (#18888).
    #[serde(default)]
    pub attempts: Vec<String>,
    pub pr_url: Option<String>,
    /// Why no pull request exists, as `<status>: <reason>`. Non-null whenever
    /// `pr_url` is null on a terminal run; null whenever `pr_url` is set.
    pub why_no_pr: Option<String>,
    /// The agent's per-item delivery accounting against the task's checklist,
    /// from its review form. Empty when the agent reported none. Any
    /// `not_delivered` item makes the Cook status `succeeded_partial`.
    #[serde(default)]
    pub delivery: Vec<CookDeliveryItem>,
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
            attempts: Vec::new(),
            pr_url,
            why_no_pr,
            delivery: Vec::new(),
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
        if self.attempts.len() > COOK_SUMMARY_ATTEMPT_LIMIT {
            let excess = self.attempts.len() - COOK_SUMMARY_ATTEMPT_LIMIT;
            self.attempts.drain(..excess);
        }
        for attempt in &mut self.attempts {
            if attempt.chars().count() > COOK_SUMMARY_ATTEMPT_MAX_CHARS {
                let kept: String = attempt
                    .chars()
                    .take(COOK_SUMMARY_ATTEMPT_MAX_CHARS - 3)
                    .collect();
                *attempt = format!("{kept}...");
            }
        }
        for gate in &mut self.gates {
            gate.failing_tests.truncate(COOK_SUMMARY_FAILING_TEST_LIMIT);
        }
        // Undelivered items are the accounting's point: keep them first.
        self.delivery.sort_by_key(CookDeliveryItem::is_delivered);
        self.delivery.truncate(COOK_SUMMARY_DELIVERY_LIMIT);
        for item in &mut self.delivery {
            item.item = bounded_text(&item.item);
            item.reason = item.reason.as_deref().map(bounded_text);
        }
        while self.serialized_len() > COOK_SUMMARY_MAX_BYTES {
            if let Some(gate) = self
                .gates
                .iter_mut()
                .filter(|gate| !gate.failing_tests.is_empty())
                .max_by_key(|gate| gate.failing_tests.len())
            {
                gate.failing_tests.pop();
            } else if self.attempts.len() > 1 {
                // Keep the latest attempt; drop the oldest history first.
                self.attempts.remove(0);
            } else if self
                .delivery
                .last()
                .is_some_and(CookDeliveryItem::is_delivered)
            {
                self.delivery.pop();
            } else if self.gates.pop().is_none() && self.delivery.pop().is_none() {
                break;
            }
        }
        self
    }

    /// Whether the agent reported any task item as not delivered.
    pub fn has_undelivered_items(&self) -> bool {
        self.delivery.iter().any(|item| !item.is_delivered())
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
        if self.delivery.len() > COOK_SUMMARY_DELIVERY_LIMIT
            || self.delivery.iter().any(|item| {
                item.item.chars().count() > COOK_SUMMARY_DELIVERY_TEXT_MAX_CHARS
                    || item.reason.as_ref().is_some_and(|reason| {
                        reason.chars().count() > COOK_SUMMARY_DELIVERY_TEXT_MAX_CHARS
                    })
            })
        {
            return Err("cook summary exceeds its delivery bounds".to_string());
        }
        if self.gates.len() > COOK_SUMMARY_GATE_LIMIT
            || self.attempts.len() > COOK_SUMMARY_ATTEMPT_LIMIT
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

fn bounded_text(text: &str) -> String {
    if text.chars().count() <= COOK_SUMMARY_DELIVERY_TEXT_MAX_CHARS {
        return text.to_string();
    }
    let kept: String = text
        .chars()
        .take(COOK_SUMMARY_DELIVERY_TEXT_MAX_CHARS - 3)
        .collect();
    format!("{kept}...")
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
            attempts: vec!["attempt 1: abc · gates cargo test=failed · discarded".to_string()],
            pr_url: None,
            why_no_pr: Some("candidate_recoverable: publication not attempted".to_string()),
            delivery: vec![CookDeliveryItem {
                item: "Handle empty input".to_string(),
                status: CookDeliveryItemStatus::NotDelivered,
                reason: Some("out of scope".to_string()),
            }],
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
            "attempts",
            "pr_url",
            "why_no_pr",
            "delivery",
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

    #[test]
    fn delivery_accounting_is_bounded_and_keeps_undelivered_items() {
        let mut summary = CookSummary::unobserved(
            "succeeded_partial",
            true,
            Some("https://github.com/o/r/pull/1".to_string()),
            "unused",
        );
        summary.delivery = (0..40)
            .map(|index| CookDeliveryItem {
                item: format!("item {index} {}", "z".repeat(400)),
                status: if index % 10 == 9 {
                    CookDeliveryItemStatus::NotDelivered
                } else {
                    CookDeliveryItemStatus::Delivered
                },
                reason: (index % 10 == 9).then(|| "r".repeat(400)),
            })
            .collect();
        assert!(summary.validate().is_err());
        let summary = summary.bounded();
        summary.validate().expect("bounded delivery fits");
        assert!(summary.has_undelivered_items());
        assert!(!summary.delivery[0].is_delivered());
        assert_eq!(
            summary
                .delivery
                .iter()
                .filter(|item| !item.is_delivered())
                .count(),
            4,
            "every undelivered item survives the bound"
        );
    }
}
