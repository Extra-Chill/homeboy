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

fn fuzz_evidence_contract_schema() -> String {
    FUZZ_EVIDENCE_CONTRACT_SCHEMA.to_string()
}

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
    /// root (`..` traversal or an absolute path outside the base). Previously
    /// these were silently discarded.
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
    #[serde(default = "fuzz_evidence_contract_schema")]
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
            schema: fuzz_evidence_contract_schema(),
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
    #[serde(default = "fuzz_evidence_contract_schema")]
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
            schema: fuzz_evidence_contract_schema(),
            complete: true,
            violations: Vec::new(),
        }
    }

    pub fn from_violations(violations: Vec<FuzzEvidenceViolation>) -> Self {
        Self {
            schema: fuzz_evidence_contract_schema(),
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

    /// Read a contract back off persisted run metadata.
    ///
    /// Prefers the structured `evidence_contract` member written by the
    /// producer. Runs recorded before that member existed only carry
    /// `missing_artifact_refs` and a collapsed `results_error` string, so
    /// those are reconstructed into the same vocabulary rather than reported
    /// as absent. The `schema` member is always restamped by this reader, so a
    /// payload cannot claim a schema it does not satisfy.
    pub fn from_run_metadata(metadata: &Value) -> Self {
        if let Some(value) = metadata.get("evidence_contract") {
            if let Ok(mut contract) = serde_json::from_value::<Self>(value.clone()) {
                contract.schema = fuzz_evidence_contract_schema();
                contract.complete = contract.violations.is_empty();
                return contract;
            }
        }
        Self::from_legacy_run_metadata(metadata)
    }

    /// Reconstruct the vocabulary from the pre-taxonomy metadata members.
    fn from_legacy_run_metadata(metadata: &Value) -> Self {
        let mut violations = Vec::new();
        if let Some(refs) = metadata
            .get("missing_artifact_refs")
            .and_then(Value::as_array)
        {
            for declared_ref in refs.iter().filter_map(Value::as_str) {
                violations.push(
                    FuzzEvidenceViolation::new(
                        FuzzEvidenceViolationCode::ArtifactRefMissing,
                        format!(
                            "declared artifact `{declared_ref}` was absent from the fuzz artifact root"
                        ),
                    )
                    .with_declared_ref(declared_ref)
                    .with_producer_contract(FUZZ_ARTIFACT_ROOT_PRODUCER_CONTRACT),
                );
            }
        }
        // The legacy `results_error` collapsed five distinct channels into one
        // string. Promote it only when it is not already represented by the
        // missing-ref list and is not a gate failure wearing the same field.
        // Classify what is left as `Unknown` rather than guessing a code from
        // substring sniffing.
        if violations.is_empty() {
            if let Some(error) = metadata
                .get("results_error")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|error| !error.is_empty())
                .filter(|error| !is_legacy_gate_failure_message(error))
            {
                violations.push(FuzzEvidenceViolation::new(
                    FuzzEvidenceViolationCode::Unknown,
                    error,
                ));
            }
        }
        Self::from_violations(violations)
    }
}

/// The exact prefix Homeboy itself writes into the legacy collapsed
/// `results_error` member for an expected-metric gate failure.
///
/// Matching it is matching our own emitted format, not sniffing arbitrary
/// runner text. A gate that did not hold is a statement about declared pass
/// criteria, so reconstructing it as an evidence-contract violation would
/// blame the producer for the very collapse this taxonomy removes. New runs
/// never reach this path: they carry a structured `evidence_contract` that
/// excludes gate failures at production time.
const LEGACY_GATE_FAILURE_PREFIX: &str = "fuzz expected metric gate(s) failed";

fn is_legacy_gate_failure_message(error: &str) -> bool {
    error.starts_with(LEGACY_GATE_FAILURE_PREFIX)
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
    fn passing_cases_with_a_missing_artifact_classify_as_evidence_contract_failure() {
        let evidence = FuzzEvidenceContract::from_violations(vec![missing_ref("results.json")]);
        let classification = classify_fuzz_failure(&FuzzFailureSignals {
            evidence: &evidence,
            failed_case_ids: &[],
            open_findings: 0,
            failed_gate_ids: &[],
            campaign_present: true,
        });

        assert_eq!(
            classification.domain,
            FuzzFailureDomain::EvidenceContractFailure
        );
        // The reported #10513 regression: the workload verdict must survive.
        assert_eq!(classification.workload_verdict, FuzzWorkloadVerdict::Passed);
        assert_eq!(
            classification.evidence_verdict,
            FuzzEvidenceVerdict::Incomplete
        );
    }

    #[test]
    fn an_open_finding_outranks_an_incomplete_evidence_contract() {
        let evidence = FuzzEvidenceContract::from_violations(vec![missing_ref("results.json")]);
        let classification = classify_fuzz_failure(&FuzzFailureSignals {
            evidence: &evidence,
            failed_case_ids: &[],
            open_findings: 1,
            failed_gate_ids: &[],
            campaign_present: true,
        });

        assert_eq!(classification.domain, FuzzFailureDomain::ProductFinding);
        assert_eq!(classification.workload_verdict, FuzzWorkloadVerdict::Failed);
        // Both verdicts stay visible; the domain only says which one leads.
        assert_eq!(
            classification.evidence_verdict,
            FuzzEvidenceVerdict::Incomplete
        );
    }

    #[test]
    fn a_failed_gate_with_complete_evidence_is_a_gate_failure() {
        let evidence = FuzzEvidenceContract::satisfied();
        let classification = classify_fuzz_failure(&FuzzFailureSignals {
            evidence: &evidence,
            failed_case_ids: &[],
            open_findings: 0,
            failed_gate_ids: &["no-open-findings".to_string()],
            campaign_present: true,
        });

        assert_eq!(classification.domain, FuzzFailureDomain::GateFailure);
        assert_eq!(classification.workload_verdict, FuzzWorkloadVerdict::Passed);
        assert_eq!(
            classification.evidence_verdict,
            FuzzEvidenceVerdict::Complete
        );
    }

    #[test]
    fn a_failed_case_is_a_workload_failure() {
        let evidence = FuzzEvidenceContract::satisfied();
        let classification = classify_fuzz_failure(&FuzzFailureSignals {
            evidence: &evidence,
            failed_case_ids: &["case-17".to_string()],
            open_findings: 0,
            failed_gate_ids: &[],
            campaign_present: true,
        });

        assert_eq!(classification.domain, FuzzFailureDomain::WorkloadFailure);
        assert_eq!(classification.workload_verdict, FuzzWorkloadVerdict::Failed);
    }

    #[test]
    fn an_absent_campaign_leaves_the_workload_verdict_unknown() {
        let evidence = FuzzEvidenceContract::satisfied();
        let classification = classify_fuzz_failure(&FuzzFailureSignals {
            evidence: &evidence,
            failed_case_ids: &[],
            open_findings: 0,
            failed_gate_ids: &[],
            campaign_present: false,
        });

        assert_eq!(classification.domain, FuzzFailureDomain::WorkloadFailure);
        assert_eq!(
            classification.workload_verdict,
            FuzzWorkloadVerdict::Unknown
        );
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
    fn summary_counts_the_remaining_violations() {
        let evidence = FuzzEvidenceContract::from_violations(vec![
            missing_ref("results.json"),
            missing_ref("case-log.json"),
        ]);

        let summary = evidence.summary().expect("summary");

        assert!(summary.contains("results.json"));
        assert!(summary.contains("+1 further"));
    }

    #[test]
    fn a_satisfied_contract_has_no_summary() {
        assert!(FuzzEvidenceContract::satisfied().summary().is_none());
    }

    #[test]
    fn unknown_labels_deserialize_to_unknown_rather_than_failing() {
        let domain: FuzzFailureDomain =
            serde_json::from_value(serde_json::json!("brand_new_domain")).expect("domain");
        assert_eq!(domain, FuzzFailureDomain::Unknown);

        let code: FuzzEvidenceViolationCode =
            serde_json::from_value(serde_json::json!("brand_new_code")).expect("code");
        assert_eq!(code, FuzzEvidenceViolationCode::Unknown);

        let verdict: FuzzWorkloadVerdict =
            serde_json::from_value(serde_json::json!("indeterminate")).expect("verdict");
        assert_eq!(verdict, FuzzWorkloadVerdict::Unknown);
    }

    #[test]
    fn violations_tolerate_unknown_members_from_a_newer_producer() {
        let violation: FuzzEvidenceViolation = serde_json::from_value(serde_json::json!({
            "code": "artifact_ref_missing",
            "message": "absent",
            "declared_ref": "results.json",
            "future_member": { "nested": true }
        }))
        .expect("violation");

        assert_eq!(
            violation.code,
            FuzzEvidenceViolationCode::ArtifactRefMissing
        );
        assert_eq!(violation.declared_ref.as_deref(), Some("results.json"));
        assert_eq!(violation.schema, FUZZ_EVIDENCE_CONTRACT_SCHEMA);
    }

    #[test]
    fn run_metadata_prefers_the_structured_contract() {
        let metadata = serde_json::json!({
            "evidence_contract": {
                "complete": false,
                "violations": [{
                    "code": "artifact_ref_unresolvable",
                    "message": "escapes the artifact root",
                    "declared_ref": "../outside.json"
                }]
            },
            "missing_artifact_refs": ["ignored.json"]
        });

        let contract = FuzzEvidenceContract::from_run_metadata(&metadata);

        assert!(!contract.complete);
        assert_eq!(contract.violations.len(), 1);
        assert_eq!(
            contract.violations[0].code,
            FuzzEvidenceViolationCode::ArtifactRefUnresolvable
        );
    }

    #[test]
    fn run_metadata_reconstructs_legacy_missing_artifact_refs() {
        let metadata = serde_json::json!({
            "missing_artifact_refs": ["results.json"],
            "results_error": "fuzz campaign references artifact path(s) missing from HOMEBOY_FUZZ_ARTIFACTS_DIR: results.json"
        });

        let contract = FuzzEvidenceContract::from_run_metadata(&metadata);

        assert!(!contract.complete);
        assert_eq!(contract.violations.len(), 1);
        assert_eq!(
            contract.violations[0].code,
            FuzzEvidenceViolationCode::ArtifactRefMissing
        );
        assert_eq!(
            contract.violations[0].declared_ref.as_deref(),
            Some("results.json")
        );
    }

    #[test]
    fn a_legacy_results_error_alone_is_not_guessed_into_a_specific_code() {
        let metadata = serde_json::json!({ "results_error": "runner exploded" });

        let contract = FuzzEvidenceContract::from_run_metadata(&metadata);

        assert!(!contract.complete);
        assert_eq!(
            contract.violations[0].code,
            FuzzEvidenceViolationCode::Unknown
        );
        assert_eq!(contract.violations[0].message, "runner exploded");
    }

    /// A legacy run whose only failure was a gate must not be reconstructed as
    /// an evidence-contract violation. `results_error` collapsed both, so the
    /// reader has to un-collapse it or it re-creates the reported bug in the
    /// opposite direction.
    #[test]
    fn a_legacy_gate_failure_in_results_error_is_not_an_evidence_violation() {
        let metadata = serde_json::json!({
            "results_error": "fuzz expected metric gate(s) failed: p95_ms expected 500 observed 900"
        });

        let contract = FuzzEvidenceContract::from_run_metadata(&metadata);

        assert!(contract.complete);
        assert!(contract.violations.is_empty());
    }

    #[test]
    fn run_metadata_without_evidence_members_is_satisfied() {
        let contract = FuzzEvidenceContract::from_run_metadata(&serde_json::json!({
            "success": true
        }));

        assert!(contract.complete);
        assert!(contract.violations.is_empty());
    }

    #[test]
    fn the_reader_restamps_the_schema_a_payload_claims() {
        let metadata = serde_json::json!({
            "evidence_contract": {
                "schema": "attacker/not-the-contract/v9",
                "complete": true,
                "violations": []
            }
        });

        let contract = FuzzEvidenceContract::from_run_metadata(&metadata);

        assert_eq!(contract.schema, FUZZ_EVIDENCE_CONTRACT_SCHEMA);
    }

    #[test]
    fn a_payload_claiming_completeness_with_violations_is_corrected() {
        let metadata = serde_json::json!({
            "evidence_contract": {
                "complete": true,
                "violations": [{
                    "code": "artifact_ref_missing",
                    "message": "absent"
                }]
            }
        });

        let contract = FuzzEvidenceContract::from_run_metadata(&metadata);

        assert!(!contract.complete);
    }
}
