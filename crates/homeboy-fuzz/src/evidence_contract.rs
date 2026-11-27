//! Fuzz failure taxonomy.
//!
//! "The evidence contract was violated" and "the workload under test failed"
//! are different events with different owners. The first is a defect in the
//! producer (a rig, a runner, or Homeboy itself) that failed to deliver the
//! evidence it declared; the second is a finding about the code under test.
//! Collapsing them makes harness packaging errors read as product failures.
//!
//! This module owns the shared vocabulary:
//!
//! * [`FuzzFailureDomain`] — which of the four failure families a failed run
//!   belongs to.
//! * [`FuzzEvidenceViolation`] — one specific way an evidence contract was
//!   broken, carrying the declared reference, the base it was resolved
//!   against, and the producer contract that owed it.
//! * [`FuzzEvidenceContract`] — the campaign-level completeness verdict.
//! * [`classify_fuzz_failure`] — the precedence rule that keeps a workload
//!   verdict independent of an evidence verdict.
//!
//! Both enums carry an `Unknown` catch-all so a label minted by a newer
//! producer deserializes to "not owned by this binary" rather than failing the
//! whole payload. Nothing here uses `deny_unknown_fields`: fuzz evidence
//! crosses the homeboy/homeboy-extensions boundary and those ship separately,
//! so every member is additive in both directions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::schemas::FUZZ_EVIDENCE_CONTRACT_SCHEMA;

/// Which family a failed fuzz run belongs to.
///
/// These are not severities and they are not ordered by badness. They are
/// *owners*: a `ProductFinding` belongs to the code under test, a
/// `WorkloadFailure` to the runner or the harness, a `GateFailure` to the
/// declared pass criteria, and an `EvidenceContractFailure` to whoever
/// promised the evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FuzzFailureDomain {
    /// The workload executed and the product misbehaved. This is a discovery.
    ProductFinding,
    /// The workload or its runner could not execute correctly.
    WorkloadFailure,
    /// Execution and evidence were fine; a declared gate was not met.
    GateFailure,
    /// Declared evidence was not delivered. Says nothing about the product.
    EvidenceContractFailure,
    /// A domain minted by a producer this binary does not own. Never guessed.
    #[serde(other)]
    Unknown,
}

impl FuzzFailureDomain {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProductFinding => "product_finding",
            Self::WorkloadFailure => "workload_failure",
            Self::GateFailure => "gate_failure",
            Self::EvidenceContractFailure => "evidence_contract_failure",
            Self::Unknown => "unknown",
        }
    }
}

/// Did the workload itself pass, fail, or is it unjudgeable?
///
/// `Unknown` is load-bearing: when no campaign was produced there is nothing
/// to judge, and reporting `Failed` would assert a discovery about the product
/// that was never made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FuzzWorkloadVerdict {
    Passed,
    Failed,
    #[serde(other)]
    Unknown,
}

impl FuzzWorkloadVerdict {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

/// Was every declared piece of evidence delivered?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FuzzEvidenceVerdict {
    Complete,
    Incomplete,
    #[serde(other)]
    Unknown,
}

impl FuzzEvidenceVerdict {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
            Self::Unknown => "unknown",
        }
    }
}

/// One specific way a producer broke its evidence contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FuzzEvidenceViolationCode {
    /// A declared artifact path resolved inside the artifact root but nothing
    /// is there.
    ArtifactRefMissing,
    /// A declared artifact path is local-looking but escapes the artifact
    /// root (`..` traversal or an absolute path outside the base).
    ArtifactRefUnresolvable,
    /// A declared artifact path exists but is the wrong kind — a directory
    /// where the contract declared a file, or the reverse.
    ArtifactRefWrongKind,
    /// A declared artifact path exists and is the right kind but cannot be
    /// read.
    ArtifactRefUnreadable,
    /// A `--require-*` artifact the campaign never declared at all.
    RequiredArtifactAbsent,
    /// The runner emitted a result file Homeboy could not normalize.
    ResultsUnparseable,
    /// A required artifact post-process step failed.
    RequiredPostprocessFailed,
    /// An expectation Homeboy could not resolve to a known code.
    #[serde(other)]
    Unknown,
}

impl FuzzEvidenceViolationCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ArtifactRefMissing => "artifact_ref_missing",
            Self::ArtifactRefUnresolvable => "artifact_ref_unresolvable",
            Self::ArtifactRefWrongKind => "artifact_ref_wrong_kind",
            Self::ArtifactRefUnreadable => "artifact_ref_unreadable",
            Self::RequiredArtifactAbsent => "required_artifact_absent",
            Self::ResultsUnparseable => "results_unparseable",
            Self::RequiredPostprocessFailed => "required_postprocess_failed",
            Self::Unknown => "unknown",
        }
    }

    /// True when the violation concerns a specific declared artifact
    /// reference, so a reader can lead the root cause with the path rather
    /// than with runner output.
    pub const fn is_artifact_ref(self) -> bool {
        matches!(
            self,
            Self::ArtifactRefMissing
                | Self::ArtifactRefUnresolvable
                | Self::ArtifactRefWrongKind
                | Self::ArtifactRefUnreadable
        )
    }
}

/// One evidence-contract violation, with everything a reviewer needs to route
/// it to the producer that owes the evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzEvidenceViolation {
    pub schema: String,
    pub code: FuzzEvidenceViolationCode,
    /// Statement of what was promised and not delivered.
    pub message: String,
    /// The reference exactly as the producer declared it, before resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_ref: Option<String>,
    /// The base the declared reference was resolved against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_base: Option<String>,
    /// The contract or environment channel that owns producing this evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_contract: Option<String>,
}

impl FuzzEvidenceViolation {
    pub fn new(code: FuzzEvidenceViolationCode, message: impl Into<String>) -> Self {
        Self {
            schema: FUZZ_EVIDENCE_CONTRACT_SCHEMA.to_string(),
            code,
            message: message.into(),
            declared_ref: None,
            resolution_base: None,
            producer_contract: None,
        }
    }

    pub fn with_declared_ref(mut self, declared_ref: impl Into<String>) -> Self {
        self.declared_ref = Some(declared_ref.into());
        self
    }

    pub fn with_resolution_base(mut self, base: impl Into<String>) -> Self {
        self.resolution_base = Some(base.into());
        self
    }

    pub fn with_producer_contract(mut self, contract: impl Into<String>) -> Self {
        self.producer_contract = Some(contract.into());
        self
    }

    /// One-line root-cause rendering that leads with the declared reference
    /// and its resolution base rather than with runner output.
    pub fn root_cause_line(&self) -> String {
        let mut line = format!("[{}] {}", self.code.as_str(), self.message);
        if let Some(declared_ref) = self.declared_ref.as_deref() {
            line.push_str(&format!(" (declared ref: {declared_ref}"));
            if let Some(base) = self.resolution_base.as_deref() {
                line.push_str(&format!(", resolved against: {base}"));
            }
            if let Some(contract) = self.producer_contract.as_deref() {
                line.push_str(&format!(", owed by: {contract}"));
            }
            line.push(')');
        } else if let Some(contract) = self.producer_contract.as_deref() {
            line.push_str(&format!(" (owed by: {contract})"));
        }
        line
    }
}

/// Campaign-level evidence completeness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzEvidenceContract {
    pub schema: String,
    /// Every declared piece of evidence was delivered.
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<FuzzEvidenceViolation>,
}

impl Default for FuzzEvidenceContract {
    fn default() -> Self {
        Self::satisfied()
    }
}

impl FuzzEvidenceContract {
    /// A contract with nothing outstanding.
    pub fn satisfied() -> Self {
        Self {
            schema: FUZZ_EVIDENCE_CONTRACT_SCHEMA.to_string(),
            complete: true,
            violations: Vec::new(),
        }
    }

    pub fn from_violations(violations: Vec<FuzzEvidenceViolation>) -> Self {
        Self {
            schema: FUZZ_EVIDENCE_CONTRACT_SCHEMA.to_string(),
            complete: violations.is_empty(),
            violations,
        }
    }

    pub fn verdict(&self) -> FuzzEvidenceVerdict {
        if self.complete {
            FuzzEvidenceVerdict::Complete
        } else {
            FuzzEvidenceVerdict::Incomplete
        }
    }

    /// Root-cause lines for every violation, most specific first: artifact
    /// reference violations name a concrete path, so they lead.
    pub fn root_cause_lines(&self) -> Vec<String> {
        let mut artifact_lines = Vec::new();
        let mut other_lines = Vec::new();
        for violation in &self.violations {
            if violation.code.is_artifact_ref() {
                artifact_lines.push(violation.root_cause_line());
            } else {
                other_lines.push(violation.root_cause_line());
            }
        }
        artifact_lines.extend(other_lines);
        artifact_lines
    }

    /// Single-sentence summary suitable for a compact diagnosis.
    pub fn summary(&self) -> Option<String> {
        let first = self.violations.first()?;
        Some(match self.violations.len() {
            1 => first.root_cause_line(),
            count => format!(
                "{} (+{} further evidence-contract violation(s))",
                first.root_cause_line(),
                count - 1
            ),
        })
    }

    /// Validate the producer's current structured contract before storage or use.
    pub fn validate(&self) -> homeboy_core::Result<()> {
        let error = if self.schema != FUZZ_EVIDENCE_CONTRACT_SCHEMA {
            Some("unsupported fuzz evidence contract schema")
        } else if self.complete && !self.violations.is_empty() {
            Some("complete is true but violations were declared")
        } else if !self.complete && self.violations.is_empty() {
            Some("complete is false but no violation was declared")
        } else if self.violations.iter().any(|violation| {
            violation.schema != FUZZ_EVIDENCE_CONTRACT_SCHEMA || violation.message.trim().is_empty()
        }) {
            Some("every violation must use the supported schema and carry a non-empty message")
        } else {
            None
        };
        match error {
            None => Ok(()),
            Some(error) => Err(homeboy_core::Error::new(
                homeboy_core::ErrorCode::ValidationInvalidArgument,
                "Contract validation failed",
                serde_json::json!({
                    "schema": FUZZ_EVIDENCE_CONTRACT_SCHEMA,
                    "valid": false,
                    "error": error,
                }),
            )),
        }
    }

    /// Read the current structured contract; unsupported or malformed evidence
    /// is an explicit error, never a reconstructed or implicitly satisfied proof.
    pub fn from_run_metadata(metadata: &Value) -> homeboy_core::Result<Self> {
        let value = metadata.get("evidence_contract").ok_or_else(|| {
            homeboy_core::Error::validation_invalid_argument(
                "evidence_contract",
                "fuzz run has no structured evidence contract",
                None,
                None,
            )
        })?;
        let contract: Self = serde_json::from_value(value.clone()).map_err(|error| {
            homeboy_core::Error::validation_invalid_argument(
                "evidence_contract",
                format!("invalid structured fuzz evidence contract: {error}"),
                None,
                None,
            )
        })?;
        contract.validate()?;
        Ok(contract)
    }
}

/// The producer channel that owes artifacts declared by a campaign.
pub const FUZZ_ARTIFACT_ROOT_PRODUCER_CONTRACT: &str = "HOMEBOY_FUZZ_ARTIFACTS_DIR";
/// The producer channel that owes the campaign result file.
pub const FUZZ_RESULTS_FILE_PRODUCER_CONTRACT: &str = "HOMEBOY_FUZZ_RESULTS_FILE";

/// The observed facts a classification decision is made from.
#[derive(Debug, Clone, Copy)]
pub struct FuzzFailureSignals<'a> {
    pub evidence: &'a FuzzEvidenceContract,
    /// Case ids the campaign itself reported as failed or errored.
    pub failed_case_ids: &'a [String],
    /// Findings the campaign left open.
    pub open_findings: u64,
    /// Gate ids the gate evaluation reported as not passed.
    pub failed_gate_ids: &'a [String],
    /// A campaign was produced and parsed. When false there is nothing to
    /// judge the workload by.
    pub campaign_present: bool,
}

/// A failed run's domain plus the two verdicts that must not be collapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzFailureClassification {
    pub domain: FuzzFailureDomain,
    pub workload_verdict: FuzzWorkloadVerdict,
    pub evidence_verdict: FuzzEvidenceVerdict,
}

/// Assign a failed run to exactly one domain while keeping the workload and
/// evidence verdicts independent.
///
/// Precedence is most-specific-first. Evidence completeness never downgrades
/// the workload verdict: a campaign whose cases all passed keeps
/// [`FuzzWorkloadVerdict::Passed`] even when the run fails strict validation
/// because a declared artifact is absent. That separation is the whole point —
/// a harness packaging error must not read as a product failure.
pub fn classify_fuzz_failure(signals: &FuzzFailureSignals<'_>) -> FuzzFailureClassification {
    let workload_verdict = if !signals.campaign_present {
        FuzzWorkloadVerdict::Unknown
    } else if signals.open_findings > 0 || !signals.failed_case_ids.is_empty() {
        FuzzWorkloadVerdict::Failed
    } else {
        FuzzWorkloadVerdict::Passed
    };
    let evidence_verdict = signals.evidence.verdict();

    let domain = if signals.open_findings > 0 {
        FuzzFailureDomain::ProductFinding
    } else if !signals.failed_case_ids.is_empty() {
        FuzzFailureDomain::WorkloadFailure
    } else if !signals.failed_gate_ids.is_empty() {
        FuzzFailureDomain::GateFailure
    } else if !signals.evidence.complete {
        FuzzFailureDomain::EvidenceContractFailure
    } else if !signals.campaign_present {
        // The runner produced nothing parseable and declared nothing missing.
        FuzzFailureDomain::WorkloadFailure
    } else {
        FuzzFailureDomain::Unknown
    };

    FuzzFailureClassification {
        domain,
        workload_verdict,
        evidence_verdict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing_ref(path: &str) -> FuzzEvidenceViolation {
        FuzzEvidenceViolation::new(
            FuzzEvidenceViolationCode::ArtifactRefMissing,
            format!("declared artifact `{path}` was absent from the fuzz artifact root"),
        )
        .with_declared_ref(path)
        .with_resolution_base("/tmp/fuzz-artifacts")
        .with_producer_contract(FUZZ_ARTIFACT_ROOT_PRODUCER_CONTRACT)
    }

    #[test]
    fn classification_keeps_workload_and_evidence_verdicts_independent() {
        let incomplete = FuzzEvidenceContract::from_violations(vec![missing_ref("results.json")]);
        let complete = FuzzEvidenceContract::satisfied();
        let failed_case = vec!["case-17".to_string()];
        let failed_gate = vec!["no-open-findings".to_string()];
        let none: Vec<String> = Vec::new();

        // (label, evidence, failed cases, open findings, failed gates, campaign present,
        //  expected domain, expected workload verdict, expected evidence verdict)
        let cases = [
            (
                // The reported #10513 regression: the workload verdict must survive.
                "missing artifact with passing cases",
                &incomplete,
                &none,
                0,
                &none,
                true,
                FuzzFailureDomain::EvidenceContractFailure,
                FuzzWorkloadVerdict::Passed,
                FuzzEvidenceVerdict::Incomplete,
            ),
            (
                "open finding outranks incomplete evidence",
                &incomplete,
                &none,
                1,
                &none,
                true,
                FuzzFailureDomain::ProductFinding,
                FuzzWorkloadVerdict::Failed,
                FuzzEvidenceVerdict::Incomplete,
            ),
            (
                "failed gate with complete evidence",
                &complete,
                &none,
                0,
                &failed_gate,
                true,
                FuzzFailureDomain::GateFailure,
                FuzzWorkloadVerdict::Passed,
                FuzzEvidenceVerdict::Complete,
            ),
            (
                "failed case",
                &complete,
                &failed_case,
                0,
                &none,
                true,
                FuzzFailureDomain::WorkloadFailure,
                FuzzWorkloadVerdict::Failed,
                FuzzEvidenceVerdict::Complete,
            ),
            (
                "absent campaign",
                &complete,
                &none,
                0,
                &none,
                false,
                FuzzFailureDomain::WorkloadFailure,
                FuzzWorkloadVerdict::Unknown,
                FuzzEvidenceVerdict::Complete,
            ),
        ];

        for (
            label,
            evidence,
            failed_case_ids,
            open_findings,
            failed_gate_ids,
            campaign_present,
            domain,
            workload,
            evidence_verdict,
        ) in cases
        {
            let classification = classify_fuzz_failure(&FuzzFailureSignals {
                evidence,
                failed_case_ids,
                open_findings,
                failed_gate_ids,
                campaign_present,
            });
            assert_eq!(
                classification,
                FuzzFailureClassification {
                    domain,
                    workload_verdict: workload,
                    evidence_verdict,
                },
                "{label}"
            );
        }
    }

    #[test]
    fn root_cause_lines_lead_with_the_artifact_reference() {
        let evidence = FuzzEvidenceContract::from_violations(vec![
            FuzzEvidenceViolation::new(
                FuzzEvidenceViolationCode::ResultsUnparseable,
                "runner result file is not valid JSON",
            )
            .with_producer_contract(FUZZ_RESULTS_FILE_PRODUCER_CONTRACT),
            missing_ref("results.json"),
        ]);

        let lines = evidence.root_cause_lines();

        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("artifact_ref_missing"));
        assert!(lines[0].contains("results.json"));
        assert!(lines[0].contains("/tmp/fuzz-artifacts"));
        assert!(lines[0].contains("HOMEBOY_FUZZ_ARTIFACTS_DIR"));
        assert!(lines[1].contains("results_unparseable"));
    }

    #[test]
    fn summary_leads_with_the_first_violation_and_counts_the_rest() {
        assert!(FuzzEvidenceContract::satisfied().summary().is_none());

        let evidence = FuzzEvidenceContract::from_violations(vec![
            missing_ref("results.json"),
            missing_ref("case-log.json"),
        ]);

        let summary = evidence.summary().expect("summary");

        assert!(summary.contains("results.json"));
        assert!(summary.contains("+1 further"));
    }

    #[test]
    fn newer_producer_labels_and_members_decode_instead_of_failing() {
        let domain: FuzzFailureDomain =
            serde_json::from_value(serde_json::json!("brand_new_domain")).expect("domain");
        assert_eq!(domain, FuzzFailureDomain::Unknown);

        let verdict: FuzzWorkloadVerdict =
            serde_json::from_value(serde_json::json!("indeterminate")).expect("verdict");
        assert_eq!(verdict, FuzzWorkloadVerdict::Unknown);

        let violation: FuzzEvidenceViolation = serde_json::from_value(serde_json::json!({
            "schema": FUZZ_EVIDENCE_CONTRACT_SCHEMA,
            "code": "brand_new_code",
            "message": "absent",
            "declared_ref": "results.json",
            "future_member": { "nested": true }
        }))
        .expect("violation");
        assert_eq!(violation.code, FuzzEvidenceViolationCode::Unknown);
        assert_eq!(violation.declared_ref.as_deref(), Some("results.json"));
    }

    #[test]
    fn run_metadata_reads_the_current_structured_contract() {
        let metadata = serde_json::json!({
            "evidence_contract": {
                "schema": FUZZ_EVIDENCE_CONTRACT_SCHEMA,
                "complete": false,
                "violations": [{
                    "schema": FUZZ_EVIDENCE_CONTRACT_SCHEMA,
                    "code": "artifact_ref_unresolvable",
                    "message": "escapes the artifact root",
                    "declared_ref": "../outside.json"
                }]
            }
        });

        let contract = FuzzEvidenceContract::from_run_metadata(&metadata).expect("valid contract");

        assert!(!contract.complete);
        assert_eq!(contract.violations.len(), 1);
        assert_eq!(
            contract.violations[0].code,
            FuzzEvidenceViolationCode::ArtifactRefUnresolvable
        );
    }

    #[test]
    fn run_metadata_rejects_missing_or_untrustworthy_contracts() {
        let missing = FuzzEvidenceContract::from_run_metadata(&serde_json::json!({
            "success": true
        }))
        .expect_err("missing canonical evidence must fail");
        assert_eq!(missing.details["field"], "evidence_contract");

        let violation = serde_json::json!({
            "schema": FUZZ_EVIDENCE_CONTRACT_SCHEMA,
            "code": "artifact_ref_missing",
            "message": "absent"
        });
        let invalid = [
            (
                "unsupported schema",
                serde_json::json!({
                    "schema": "attacker/not-the-contract/v9",
                    "complete": true,
                    "violations": []
                }),
            ),
            (
                "complete with violations",
                serde_json::json!({
                    "schema": FUZZ_EVIDENCE_CONTRACT_SCHEMA,
                    "complete": true,
                    "violations": [violation]
                }),
            ),
            (
                "incomplete without violations",
                serde_json::json!({
                    "schema": FUZZ_EVIDENCE_CONTRACT_SCHEMA,
                    "complete": false,
                    "violations": []
                }),
            ),
        ];
        for (label, contract) in invalid {
            let error = FuzzEvidenceContract::from_run_metadata(
                &serde_json::json!({ "evidence_contract": contract }),
            )
            .expect_err(label);
            assert_eq!(error.details["valid"], false, "{label}");
        }
    }
}
