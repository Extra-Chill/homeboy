//! Reviewer-facing fuzz proof projection.
//!
//! A successful strict fuzz campaign already retains everything a reviewer
//! needs: exact component and rig identity, case and finding totals, gate
//! outcomes, coverage, seed, isolation, and tracker links. Until now the
//! reviewer-facing read path projected almost none of it, so posting evidence
//! on a pull request meant reading a 73 KB result envelope and running `git`
//! by hand in two worktrees.
//!
//! [`derive_fuzz_proof`] is a **read-time** derivation over the persisted run
//! record. It writes nothing: a projection that mutated stored state would
//! freeze a derivation that improves as the deriver improves. Following the
//! same discipline as the evidence manifest, [`FuzzProof::source`] is stamped
//! by this reader and never trusted from the payload, so "Homeboy derived
//! this" can never be confused with "an extension asserted this".
//!
//! Facts the run did not record are reported in [`FuzzProof::gaps`] rather
//! than guessed or silently omitted.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use homeboy_core::observation::RunRecord;

use super::contract::FuzzFindingStatus;
use super::evidence_contract::{
    classify_fuzz_failure, FuzzEvidenceContract, FuzzEvidenceVerdict, FuzzFailureDomain,
    FuzzFailureSignals, FuzzWorkloadVerdict,
};
use super::schemas::FUZZ_PROOF_SCHEMA;
use super::types::{FuzzCampaign, FuzzCase};

fn fuzz_proof_schema() -> String {
    FUZZ_PROOF_SCHEMA.to_string()
}

/// Where a proof projection came from. Stamped by the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FuzzProofSource {
    /// Derived by Homeboy from the persisted run record.
    RunMetadata,
    /// Derived by Homeboy from a retained result envelope artifact.
    ResultEnvelope,
    /// Authored by a producer and passed through.
    Authored,
    #[serde(other)]
    Unknown,
}

impl FuzzProofSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RunMetadata => "run_metadata",
            Self::ResultEnvelope => "result_envelope",
            Self::Authored => "authored",
            Self::Unknown => "unknown",
        }
    }
}

/// Exact identity of one source tree that participated in the run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzProofRevision {
    /// `component` or `rig`.
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Commit revision recorded at execution time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Deterministic content hash of the exact files used, which stays exact
    /// even when the tree was dirty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dirty: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub linked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
}

impl FuzzProofRevision {
    /// True when this revision pins the tree exactly enough to reproduce it.
    pub fn is_exact(&self) -> bool {
        self.content_hash.is_some() || (self.revision.is_some() && !self.dirty)
    }
}

/// Case outcomes for the campaign.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzProofCaseTotals {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    /// Cases whose declared status this binary does not own. Never folded into
    /// `passed`.
    pub unknown: u64,
}

/// Finding totals by status and severity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzProofFindingTotals {
    pub total: u64,
    pub open: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_status: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_severity: BTreeMap<String, u64>,
}

/// One gate that did not pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FuzzProofFailedGate {
    pub gate_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub metric: String,
    pub observed: f64,
    pub expected: f64,
}

/// Gate outcomes for the campaign.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FuzzProofGateTotals {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_gates: Vec<FuzzProofFailedGate>,
}

/// Declared versus proven coverage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzProofCoverage {
    pub declared_targets: u64,
    pub proven_targets: u64,
    pub declared_operations: u64,
    pub proven_operations: u64,
    pub complete: bool,
    /// Named coverage dimensions the campaign emitted, e.g. surface or kind
    /// selector ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimensions: Vec<String>,
}

/// What was fuzzed, with which inputs, under which placement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzProofExecution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_destructive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration: Option<String>,
    /// `lab` when the campaign was offloaded to a Lab runner, `local`
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homeboy_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// The three verdicts a reviewer needs, kept separate on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzProofVerdict {
    /// The overall strict verdict for the run.
    pub overall: bool,
    /// Did the code under test behave? Independent of evidence completeness.
    pub workload: FuzzWorkloadVerdict,
    /// Was every declared piece of evidence delivered?
    pub evidence: FuzzEvidenceVerdict,
    /// Which family a failed run belongs to. `None` for a passing run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<FuzzFailureDomain>,
}

/// The bounded reviewer projection for one fuzz run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FuzzProof {
    #[serde(default = "fuzz_proof_schema")]
    pub schema: String,
    /// Stamped by the reader. A payload claiming a different source is
    /// overwritten.
    pub source: FuzzProofSource,
    pub run_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    pub verdict: FuzzProofVerdict,
    pub component: FuzzProofRevision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig: Option<FuzzProofRevision>,
    pub execution: FuzzProofExecution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cases: Option<FuzzProofCaseTotals>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<FuzzProofFindingTotals>,
    pub gates: FuzzProofGateTotals,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<FuzzProofCoverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracker_refs: Vec<Value>,
    pub evidence: FuzzEvidenceContract,
    /// Facts a reviewer would expect that this run did not record. Stated
    /// rather than guessed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<String>,
    /// Markdown rendering suitable for a pull-request or issue comment.
    pub markdown: String,
}

/// Case outcome totals for a parsed campaign.
///
/// `FuzzCase` has no typed status member — runners carry it in the flattened
/// extras — so a status this binary does not own counts as `unknown` and is
/// never folded into `passed`.
pub fn fuzz_campaign_case_totals(campaign: &FuzzCampaign) -> FuzzProofCaseTotals {
    let mut totals = FuzzProofCaseTotals {
        total: campaign.cases.len() as u64,
        ..FuzzProofCaseTotals::default()
    };
    for case in &campaign.cases {
        match case_status(case).as_deref() {
            Some("passed" | "pass" | "ok" | "success") => totals.passed += 1,
            Some("failed" | "fail" | "error" | "errored") => totals.failed += 1,
            Some("skipped" | "skip") => totals.skipped += 1,
            _ => totals.unknown += 1,
        }
    }
    totals
}

fn case_status(case: &FuzzCase) -> Option<String> {
    case.extra
        .get("status")
        .and_then(Value::as_str)
        .or_else(|| case.metadata.get("status").and_then(Value::as_str))
        .or_else(|| case.observed.get("status").and_then(Value::as_str))
        .map(|status| status.trim().to_ascii_lowercase())
}

/// Finding totals for a parsed campaign, by status and severity.
pub fn fuzz_campaign_finding_totals(campaign: &FuzzCampaign) -> FuzzProofFindingTotals {
    let mut totals = FuzzProofFindingTotals {
        total: campaign.findings.len() as u64,
        ..FuzzProofFindingTotals::default()
    };
    for finding in &campaign.findings {
        let status = match finding.status {
            FuzzFindingStatus::Open => "open",
            FuzzFindingStatus::Confirmed => "confirmed",
            FuzzFindingStatus::Mitigated => "mitigated",
            FuzzFindingStatus::Suppressed => "suppressed",
        };
        if finding.status == FuzzFindingStatus::Open {
            totals.open += 1;
        }
        *totals.by_status.entry(status.to_string()).or_insert(0) += 1;
        let severity = finding.severity.trim();
        let severity = if severity.is_empty() {
            "unspecified"
        } else {
            severity
        };
        *totals.by_severity.entry(severity.to_string()).or_insert(0) += 1;
    }
    totals
}

/// Derive the reviewer proof for a fuzz run from its persisted record.
///
/// Returns `None` for runs that are not fuzz runs, so the generic
/// `runs proof` projection stays generic.
pub fn derive_fuzz_proof(run: &RunRecord) -> Option<FuzzProof> {
    if run.kind != "fuzz" {
        return None;
    }
    let metadata = &run.metadata_json;
    let evidence = FuzzEvidenceContract::from_run_metadata(metadata);
    let cases = case_totals(metadata);
    let findings = finding_totals(metadata);
    let gates = gate_totals(metadata);
    let coverage = coverage(metadata);
    let component = component_revision(run);
    let rig = rig_revision(run);

    let overall = metadata
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| matches!(run.status.as_str(), "pass" | "passed"));

    let failed_case_ids: Vec<String> = cases
        .as_ref()
        .filter(|totals| totals.failed > 0)
        .map(|totals| vec![format!("{} failed case(s)", totals.failed)])
        .unwrap_or_default();
    let failed_gate_ids: Vec<String> = gates
        .failed_gates
        .iter()
        .map(|gate| gate.gate_id.clone())
        .collect();
    let classification = classify_fuzz_failure(&FuzzFailureSignals {
        evidence: &evidence,
        failed_case_ids: &failed_case_ids,
        open_findings: findings.as_ref().map(|totals| totals.open).unwrap_or(0),
        failed_gate_ids: &failed_gate_ids,
        campaign_present: metadata.get("campaign_id").is_some_and(|id| !id.is_null()),
    });

    let verdict = FuzzProofVerdict {
        overall,
        workload: classification.workload_verdict,
        evidence: classification.evidence_verdict,
        failure_domain: (!overall).then_some(classification.domain),
    };

    let mut proof = FuzzProof {
        schema: fuzz_proof_schema(),
        source: FuzzProofSource::RunMetadata,
        run_id: run.id.clone(),
        status: run.status.clone(),
        campaign_id: string_member(metadata, "campaign_id"),
        verdict,
        component,
        rig,
        execution: execution(run),
        cases,
        findings,
        gates,
        coverage,
        tracker_refs: metadata
            .get("tracker_refs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        evidence,
        gaps: Vec::new(),
        markdown: String::new(),
    };
    proof.gaps = gaps(&proof);
    proof.markdown = render_markdown(&proof);
    Some(proof)
}

fn string_member(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn component_revision(run: &RunRecord) -> FuzzProofRevision {
    FuzzProofRevision {
        role: "component".to_string(),
        id: run.component_id.clone(),
        revision: run
            .git_sha
            .clone()
            .or_else(|| string_member(&run.metadata_json, "component_revision")),
        content_hash: string_member(&run.metadata_json, "component_content_hash"),
        dirty: run
            .metadata_json
            .get("component_source_dirty")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        linked: false,
        freshness: None,
    }
}

fn rig_revision(run: &RunRecord) -> Option<FuzzProofRevision> {
    let package = run.metadata_json.get("rig_package")?;
    if package.is_null() {
        return None;
    }
    Some(FuzzProofRevision {
        role: "rig".to_string(),
        id: package
            .get("rig_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| run.rig_id.clone()),
        // The installed revision is the identity the run actually executed;
        // the current revision only says where the source has drifted to since.
        revision: package
            .get("installed_source_revision")
            .and_then(Value::as_str)
            .or_else(|| {
                package
                    .get("current_source_revision")
                    .and_then(Value::as_str)
            })
            .map(str::to_string),
        content_hash: package
            .get("source_content_hash")
            .and_then(Value::as_str)
            .map(str::to_string),
        dirty: package
            .get("source_dirty")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        linked: package
            .get("linked")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        freshness: package
            .get("freshness")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn execution(run: &RunRecord) -> FuzzProofExecution {
    let metadata = &run.metadata_json;
    let settings = metadata.get("requested_settings");
    let lab = metadata.get("lab");
    FuzzProofExecution {
        workload_id: string_member(metadata, "workload_id"),
        workload_path: string_member(metadata, "workload_path"),
        seed: string_member(metadata, "seed"),
        profile: settings.and_then(|settings| {
            settings
                .get("profile")
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
        isolation: settings.and_then(|settings| {
            settings
                .get("isolation")
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
        allow_destructive: settings
            .and_then(|settings| settings.get("allow_destructive"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        max_duration: string_member(metadata, "max_duration"),
        placement: Some(if lab.is_some_and(|lab| !lab.is_null()) {
            "lab".to_string()
        } else {
            "local".to_string()
        }),
        remote_job_id: lab.and_then(|lab| {
            lab.get("remote_job_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
        homeboy_version: run.homeboy_version.clone(),
        command: run.command.clone(),
    }
}

fn case_totals(metadata: &Value) -> Option<FuzzProofCaseTotals> {
    let totals = metadata.get("case_totals")?;
    Some(FuzzProofCaseTotals {
        total: u64_member(totals, "total"),
        passed: u64_member(totals, "passed"),
        failed: u64_member(totals, "failed"),
        skipped: u64_member(totals, "skipped"),
        unknown: u64_member(totals, "unknown"),
    })
}

fn finding_totals(metadata: &Value) -> Option<FuzzProofFindingTotals> {
    let totals = metadata.get("finding_totals")?;
    Some(FuzzProofFindingTotals {
        total: u64_member(totals, "total"),
        open: u64_member(totals, "open"),
        by_status: u64_map(totals.get("by_status")),
        by_severity: u64_map(totals.get("by_severity")),
    })
}

fn u64_member(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn u64_map(value: Option<&Value>) -> BTreeMap<String, u64> {
    value
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| value.as_u64().map(|count| (key.clone(), count)))
                .collect()
        })
        .unwrap_or_default()
}

fn gate_totals(metadata: &Value) -> FuzzProofGateTotals {
    let Some(gates) = metadata.get("gates").and_then(Value::as_array) else {
        return FuzzProofGateTotals::default();
    };
    let mut totals = FuzzProofGateTotals {
        total: gates.len() as u64,
        ..FuzzProofGateTotals::default()
    };
    for gate in gates {
        let status = gate.get("status").and_then(Value::as_str).unwrap_or("");
        if status == "passed" {
            totals.passed += 1;
            continue;
        }
        totals.failed += 1;
        totals.failed_gates.push(FuzzProofFailedGate {
            gate_id: gate
                .get("gate_id")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed gate>")
                .to_string(),
            metric: gate
                .get("metric")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            observed: gate.get("observed").and_then(Value::as_f64).unwrap_or(0.0),
            expected: gate.get("expected").and_then(Value::as_f64).unwrap_or(0.0),
        });
    }
    totals
}

fn coverage(metadata: &Value) -> Option<FuzzProofCoverage> {
    let completeness = metadata.get("coverage_completeness")?;
    if !completeness
        .get("has_summary")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let declared_targets = u64_member(completeness, "declared_targets");
    let proven_targets = u64_member(completeness, "proven_targets");
    let declared_operations = u64_member(completeness, "declared_operations");
    let proven_operations = u64_member(completeness, "proven_operations");
    let mut dimensions = Vec::new();
    for key in ["surface_summaries", "kind_summaries"] {
        if let Some(entries) = completeness.get(key).and_then(Value::as_array) {
            for entry in entries {
                if let Some(id) = entry.get("id").and_then(Value::as_str) {
                    dimensions.push(format!("{key}:{id}"));
                }
            }
        }
    }
    Some(FuzzProofCoverage {
        declared_targets,
        proven_targets,
        declared_operations,
        proven_operations,
        complete: declared_targets == proven_targets && declared_operations == proven_operations,
        dimensions,
    })
}

/// Name every reviewer-relevant fact the run did not record.
fn gaps(proof: &FuzzProof) -> Vec<String> {
    let mut gaps = Vec::new();
    if !proof.component.is_exact() {
        gaps.push(
            "component revision is not pinned; the run recorded no git sha or content hash"
                .to_string(),
        );
    }
    match proof.rig.as_ref() {
        None => gaps.push("no rig package identity was recorded for this run".to_string()),
        Some(rig) if !rig.is_exact() => gaps.push(
            "rig revision is not pinned; the run recorded no revision or content hash".to_string(),
        ),
        Some(_) => {}
    }
    if proof.cases.is_none() {
        gaps.push("case totals were not recorded by the producing run".to_string());
    }
    if proof.findings.is_none() {
        gaps.push("finding totals were not recorded by the producing run".to_string());
    }
    if proof.coverage.is_none() {
        gaps.push("the campaign emitted no coverage summary".to_string());
    }
    if proof.execution.seed.is_none() {
        gaps.push(
            "no seed was recorded; the campaign may not be deterministically replayable"
                .to_string(),
        );
    }
    if proof.tracker_refs.is_empty() {
        gaps.push("no tracker reference was recorded, so this proof is not linked to a pull request or issue".to_string());
    }
    gaps
}

/// Render the proof as a Markdown comment body.
fn render_markdown(proof: &FuzzProof) -> String {
    let mut lines = vec![
        format!("## Fuzz proof — `{}`", proof.run_id),
        String::new(),
        format!(
            "| Verdict | {} |",
            if proof.verdict.overall {
                "**pass**"
            } else {
                "**fail**"
            }
        ),
        "| --- | --- |".to_string(),
        format!("| Workload | {} |", proof.verdict.workload.as_str()),
        format!("| Evidence | {} |", proof.verdict.evidence.as_str()),
    ];
    if let Some(domain) = proof.verdict.failure_domain {
        lines.push(format!("| Failure domain | `{}` |", domain.as_str()));
    }
    if let Some(campaign_id) = proof.campaign_id.as_deref() {
        lines.push(format!("| Campaign | `{campaign_id}` |"));
    }
    lines.push(format!(
        "| Component | {} |",
        revision_cell(&proof.component)
    ));
    if let Some(rig) = proof.rig.as_ref() {
        lines.push(format!("| Rig | {} |", revision_cell(rig)));
    }
    if let Some(cases) = proof.cases.as_ref() {
        lines.push(format!(
            "| Cases | {}/{} passed, {} failed, {} skipped |",
            cases.passed, cases.total, cases.failed, cases.skipped
        ));
    }
    if let Some(findings) = proof.findings.as_ref() {
        lines.push(format!(
            "| Findings | {} total, {} open |",
            findings.total, findings.open
        ));
    }
    lines.push(format!(
        "| Gates | {}/{} passed |",
        proof.gates.passed, proof.gates.total
    ));
    if let Some(coverage) = proof.coverage.as_ref() {
        lines.push(format!(
            "| Coverage | targets {}/{}, operations {}/{} |",
            coverage.proven_targets,
            coverage.declared_targets,
            coverage.proven_operations,
            coverage.declared_operations
        ));
    }
    if let Some(workload_id) = proof.execution.workload_id.as_deref() {
        lines.push(format!("| Workload id | `{workload_id}` |"));
    }
    if let Some(seed) = proof.execution.seed.as_deref() {
        lines.push(format!("| Seed | `{seed}` |"));
    }
    if let Some(isolation) = proof.execution.isolation.as_deref() {
        lines.push(format!("| Isolation | `{isolation}` |"));
    }
    if let Some(placement) = proof.execution.placement.as_deref() {
        lines.push(format!("| Placement | `{placement}` |"));
    }
    if let Some(version) = proof.execution.homeboy_version.as_deref() {
        lines.push(format!("| Homeboy | `{version}` |"));
    }

    if !proof.gates.failed_gates.is_empty() {
        lines.push(String::new());
        lines.push("### Failed gates".to_string());
        for gate in &proof.gates.failed_gates {
            lines.push(format!(
                "- `{}` — {} observed {}, expected {}",
                gate.gate_id, gate.metric, gate.observed, gate.expected
            ));
        }
    }

    if !proof.evidence.complete {
        lines.push(String::new());
        lines.push("### Evidence-contract violations".to_string());
        lines.push(
            "These are producer defects, not findings about the code under test.".to_string(),
        );
        for line in proof.evidence.root_cause_lines() {
            lines.push(format!("- {line}"));
        }
    }

    if !proof.gaps.is_empty() {
        lines.push(String::new());
        lines.push("### Not recorded".to_string());
        for gap in &proof.gaps {
            lines.push(format!("- {gap}"));
        }
    }

    lines.push(String::new());
    lines.push(format!(
        "<sub>Generated from retained run `{}` by `homeboy runs proof {} --json`.</sub>",
        proof.run_id, proof.run_id
    ));

    let mut output = lines.join("\n");
    output.push('\n');
    output
}

fn revision_cell(revision: &FuzzProofRevision) -> String {
    let mut cell = revision
        .revision
        .as_deref()
        .map(|revision| format!("`{revision}`"))
        .unwrap_or_else(|| "_unrecorded_".to_string());
    if let Some(id) = revision.id.as_deref() {
        cell = format!("{id} at {cell}");
    }
    if let Some(hash) = revision.content_hash.as_deref() {
        cell.push_str(&format!(" (content `{hash}`)"));
    }
    if revision.dirty {
        cell.push_str(" **dirty**");
    }
    cell
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fuzz_run(status: &str, metadata: Value) -> RunRecord {
        RunRecord {
            id: "studio-pr-4356-homeboy-v6".to_string(),
            kind: "fuzz".to_string(),
            component_id: Some("studio".to_string()),
            started_at: "2026-07-27T00:00:00Z".to_string(),
            finished_at: Some("2026-07-27T00:04:00Z".to_string()),
            status: status.to_string(),
            command: Some("homeboy fuzz run studio --rig studio".to_string()),
            cwd: Some("/tmp/homeboy-fixture".to_string()),
            homeboy_version: Some("0.320.0".to_string()),
            git_sha: Some("0bb440eddd8ebe53c15fe826a30c5ec13b2f58b0".to_string()),
            rig_id: Some("studio".to_string()),
            metadata_json: metadata,
        }
    }

    fn passing_metadata() -> Value {
        serde_json::json!({
            "success": true,
            "status": "passed",
            "campaign_id": "import-db-dropin",
            "workload_id": "studio.import-db-dropin",
            "workload_path": "/rigs/studio/fuzz/workload.json",
            "seed": "4356",
            "max_duration": "10m",
            "requested_settings": {
                "profile": "lab",
                "isolation": "disposable",
                "allow_destructive": false
            },
            "tracker_refs": [{ "kind": "github-pr", "id": "Automattic/studio#4356" }],
            "rig_package": {
                "rig_id": "studio",
                "installed_source_revision": "7ccbe54558d145d022ef2b88f3ba9f7e2063df5e",
                "source_content_hash": "sha256:abc",
                "source_dirty": false,
                "linked": true,
                "freshness": "verified"
            },
            "case_totals": { "total": 19, "passed": 19, "failed": 0, "skipped": 0, "unknown": 0 },
            "finding_totals": { "total": 0, "open": 0 },
            "coverage_completeness": {
                "has_summary": true,
                "declared_targets": 4,
                "proven_targets": 4,
                "declared_operations": 6,
                "proven_operations": 6,
                "surface_summaries": [{ "id": "import" }],
                "kind_summaries": [{ "id": "sqlite" }]
            },
            "gates": [
                { "gate_id": "no-open-findings", "status": "passed", "metric": "open_findings", "observed": 0.0, "expected": 0.0 },
                { "gate_id": "has-case-evidence", "status": "passed", "metric": "case_evidence", "observed": 1.0, "expected": 1.0 },
                { "gate_id": "target-coverage-complete", "status": "passed", "metric": "target_coverage", "observed": 1.0, "expected": 1.0 },
                { "gate_id": "operation-coverage-complete", "status": "passed", "metric": "operation_coverage", "observed": 1.0, "expected": 1.0 }
            ],
            "gate_status": "passed"
        })
    }

    #[test]
    fn a_non_fuzz_run_has_no_fuzz_proof() {
        let mut run = fuzz_run("pass", passing_metadata());
        run.kind = "bench".to_string();
        assert!(derive_fuzz_proof(&run).is_none());
    }

    #[test]
    fn a_successful_campaign_projects_exact_revisions_cases_gates_and_coverage() {
        let run = fuzz_run("pass", passing_metadata());

        let proof = derive_fuzz_proof(&run).expect("fuzz proof");

        assert_eq!(proof.schema, FUZZ_PROOF_SCHEMA);
        assert_eq!(proof.source, FuzzProofSource::RunMetadata);
        assert!(proof.verdict.overall);
        assert_eq!(proof.verdict.workload, FuzzWorkloadVerdict::Passed);
        assert_eq!(proof.verdict.evidence, FuzzEvidenceVerdict::Complete);
        assert!(proof.verdict.failure_domain.is_none());

        // Exact component identity — the fact `runs dossier` reported as null.
        assert_eq!(
            proof.component.revision.as_deref(),
            Some("0bb440eddd8ebe53c15fe826a30c5ec13b2f58b0")
        );
        assert!(proof.component.is_exact());
        // Exact rig identity — already in run metadata, previously unprojected.
        let rig = proof.rig.as_ref().expect("rig identity");
        assert_eq!(
            rig.revision.as_deref(),
            Some("7ccbe54558d145d022ef2b88f3ba9f7e2063df5e")
        );
        assert_eq!(rig.content_hash.as_deref(), Some("sha256:abc"));
        assert!(rig.linked);

        let cases = proof.cases.as_ref().expect("case totals");
        assert_eq!((cases.total, cases.passed, cases.failed), (19, 19, 0));
        assert_eq!(proof.findings.as_ref().expect("findings").open, 0);
        assert_eq!(proof.gates.total, 4);
        assert_eq!(proof.gates.passed, 4);
        assert!(proof.gates.failed_gates.is_empty());

        let coverage = proof.coverage.as_ref().expect("coverage");
        assert!(coverage.complete);
        assert_eq!(coverage.proven_targets, 4);
        assert_eq!(coverage.proven_operations, 6);
        assert!(coverage
            .dimensions
            .contains(&"surface_summaries:import".to_string()));

        assert_eq!(proof.execution.seed.as_deref(), Some("4356"));
        assert_eq!(proof.execution.profile.as_deref(), Some("lab"));
        assert_eq!(proof.execution.isolation.as_deref(), Some("disposable"));
        assert_eq!(proof.execution.placement.as_deref(), Some("local"));
        assert_eq!(proof.tracker_refs.len(), 1);
        assert!(proof.gaps.is_empty());
    }

    #[test]
    fn the_markdown_rendering_is_postable_and_bounded() {
        let run = fuzz_run("pass", passing_metadata());

        let proof = derive_fuzz_proof(&run).expect("fuzz proof");

        assert!(proof
            .markdown
            .starts_with("## Fuzz proof — `studio-pr-4356-homeboy-v6`"));
        assert!(proof.markdown.contains("19/19 passed"));
        assert!(proof.markdown.contains("4/4 passed"));
        assert!(proof
            .markdown
            .contains("0bb440eddd8ebe53c15fe826a30c5ec13b2f58b0"));
        assert!(proof
            .markdown
            .contains("7ccbe54558d145d022ef2b88f3ba9f7e2063df5e"));
        assert!(proof.markdown.len() < 4_000);
    }

    #[test]
    fn a_missing_artifact_keeps_the_workload_verdict_passing() {
        let mut metadata = passing_metadata();
        metadata["success"] = serde_json::json!(false);
        metadata["status"] = serde_json::json!("failed");
        metadata["missing_artifact_refs"] = serde_json::json!(["results.json"]);
        let run = fuzz_run("fail", metadata);

        let proof = derive_fuzz_proof(&run).expect("fuzz proof");

        assert!(!proof.verdict.overall);
        assert_eq!(proof.verdict.workload, FuzzWorkloadVerdict::Passed);
        assert_eq!(proof.verdict.evidence, FuzzEvidenceVerdict::Incomplete);
        assert_eq!(
            proof.verdict.failure_domain,
            Some(FuzzFailureDomain::EvidenceContractFailure)
        );
        assert!(proof.markdown.contains("Evidence-contract violations"));
        assert!(proof
            .markdown
            .contains("not findings about the code under test"));
    }

    #[test]
    fn unrecorded_facts_are_reported_as_gaps_rather_than_guessed() {
        let mut run = fuzz_run("pass", serde_json::json!({ "success": true }));
        run.git_sha = None;

        let proof = derive_fuzz_proof(&run).expect("fuzz proof");

        assert!(proof.cases.is_none());
        assert!(proof.coverage.is_none());
        assert!(proof.rig.is_none());
        let gaps = proof.gaps.join("\n");
        assert!(gaps.contains("component revision is not pinned"));
        assert!(gaps.contains("no rig package identity"));
        assert!(gaps.contains("case totals were not recorded"));
        assert!(gaps.contains("no seed was recorded"));
        assert!(gaps.contains("no tracker reference"));
        assert!(proof.markdown.contains("### Not recorded"));
    }

    #[test]
    fn a_dirty_rig_is_not_treated_as_an_exact_revision_without_a_content_hash() {
        let mut metadata = passing_metadata();
        metadata["rig_package"]["source_dirty"] = serde_json::json!(true);
        metadata["rig_package"]["source_content_hash"] = Value::Null;
        let run = fuzz_run("pass", metadata);

        let proof = derive_fuzz_proof(&run).expect("fuzz proof");

        let rig = proof.rig.as_ref().expect("rig");
        assert!(rig.dirty);
        assert!(!rig.is_exact());
        assert!(proof.gaps.iter().any(|gap| gap.contains("rig revision")));
        assert!(proof.markdown.contains("**dirty**"));
    }

    #[test]
    fn a_failed_gate_is_reported_with_observed_and_expected_values() {
        let mut metadata = passing_metadata();
        metadata["success"] = serde_json::json!(false);
        metadata["gates"][0]["status"] = serde_json::json!("failed");
        metadata["gates"][0]["observed"] = serde_json::json!(2.0);
        let run = fuzz_run("fail", metadata);

        let proof = derive_fuzz_proof(&run).expect("fuzz proof");

        assert_eq!(proof.gates.failed, 1);
        assert_eq!(proof.gates.passed, 3);
        assert_eq!(proof.gates.failed_gates[0].gate_id, "no-open-findings");
        assert_eq!(proof.gates.failed_gates[0].observed, 2.0);
        assert_eq!(
            proof.verdict.failure_domain,
            Some(FuzzFailureDomain::GateFailure)
        );
        assert!(proof.markdown.contains("### Failed gates"));
    }

    #[test]
    fn an_open_finding_is_a_product_finding_not_an_evidence_failure() {
        let mut metadata = passing_metadata();
        metadata["success"] = serde_json::json!(false);
        metadata["finding_totals"] = serde_json::json!({
            "total": 1,
            "open": 1,
            "by_severity": { "high": 1 },
            "by_status": { "open": 1 }
        });
        let run = fuzz_run("fail", metadata);

        let proof = derive_fuzz_proof(&run).expect("fuzz proof");

        assert_eq!(
            proof.verdict.failure_domain,
            Some(FuzzFailureDomain::ProductFinding)
        );
        assert_eq!(proof.verdict.workload, FuzzWorkloadVerdict::Failed);
        assert_eq!(
            proof
                .findings
                .as_ref()
                .expect("findings")
                .by_severity
                .get("high"),
            Some(&1)
        );
    }

    #[test]
    fn lab_placement_and_remote_job_are_projected_when_present() {
        let mut metadata = passing_metadata();
        metadata["lab"] = serde_json::json!({ "remote_job_id": "job-7" });
        let run = fuzz_run("pass", metadata);

        let proof = derive_fuzz_proof(&run).expect("fuzz proof");

        assert_eq!(proof.execution.placement.as_deref(), Some("lab"));
        assert_eq!(proof.execution.remote_job_id.as_deref(), Some("job-7"));
    }

    #[test]
    fn the_projection_round_trips_through_json() {
        let run = fuzz_run("pass", passing_metadata());
        let proof = derive_fuzz_proof(&run).expect("fuzz proof");

        let encoded = serde_json::to_string(&proof).expect("encode");
        let decoded: FuzzProof = serde_json::from_str(&encoded).expect("decode");

        assert_eq!(decoded, proof);
    }
}
