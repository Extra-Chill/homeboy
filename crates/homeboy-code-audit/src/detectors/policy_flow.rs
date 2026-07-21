//! Lossy policy projection and divergent decision detector.
//!
//! Extensions resolve syntax into language-neutral facts. Core only correlates
//! exact canonical identities declared by project or extension policy.

use homeboy_audit_contract::{
    AggregateDefinitionFact, AggregateProjectionFact, DecisionBranchFact, MethodCallFact,
    PolicyFlowConfig, PolicyFlowRule,
};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

struct Located<'a, T> {
    file: &'a str,
    fact: &'a T,
}

pub(crate) fn run(fingerprints: &[&FileFingerprint], config: &PolicyFlowConfig) -> Vec<Finding> {
    let mut definitions = Vec::new();
    let mut projections = Vec::new();
    let mut decisions = Vec::new();
    let mut calls = Vec::new();

    for fingerprint in fingerprints {
        if super::walker::is_test_path(&fingerprint.relative_path) {
            continue;
        }
        definitions.extend(
            fingerprint
                .aggregate_definitions
                .iter()
                .map(|fact| Located {
                    file: fingerprint.relative_path.as_str(),
                    fact,
                }),
        );
        projections.extend(
            fingerprint
                .aggregate_projections
                .iter()
                .map(|fact| Located {
                    file: fingerprint.relative_path.as_str(),
                    fact,
                }),
        );
        decisions.extend(fingerprint.decision_branches.iter().map(|fact| Located {
            file: fingerprint.relative_path.as_str(),
            fact,
        }));
        calls.extend(fingerprint.method_calls.iter().map(|fact| Located {
            file: fingerprint.relative_path.as_str(),
            fact,
        }));
    }

    definitions.sort_by(|left, right| {
        left.fact
            .type_id
            .cmp(&right.fact.type_id)
            .then(left.file.cmp(right.file))
            .then(left.fact.location.line.cmp(&right.fact.location.line))
            .then(left.fact.location.column.cmp(&right.fact.location.column))
    });
    projections.sort_by(|left, right| {
        left.fact
            .source_type_id
            .cmp(&right.fact.source_type_id)
            .then(left.fact.target_type_id.cmp(&right.fact.target_type_id))
            .then(left.fact.callable_id.cmp(&right.fact.callable_id))
            .then(left.file.cmp(right.file))
            .then(left.fact.location.line.cmp(&right.fact.location.line))
            .then(left.fact.location.column.cmp(&right.fact.location.column))
    });
    decisions.sort_by(|left, right| {
        left.fact
            .callable_id
            .cmp(&right.fact.callable_id)
            .then(left.fact.domain_type_id.cmp(&right.fact.domain_type_id))
            .then(left.file.cmp(right.file))
            .then(left.fact.location.line.cmp(&right.fact.location.line))
            .then(left.fact.location.column.cmp(&right.fact.location.column))
    });
    calls.sort_by(|left, right| {
        left.fact
            .caller_id
            .cmp(&right.fact.caller_id)
            .then(left.fact.target_method_id.cmp(&right.fact.target_method_id))
            .then(left.file.cmp(right.file))
            .then(left.fact.location.line.cmp(&right.fact.location.line))
            .then(left.fact.location.column.cmp(&right.fact.location.column))
    });

    let mut findings = Vec::new();
    for rule in &config.rules {
        findings.extend(find_rule_findings(
            rule,
            &definitions,
            &projections,
            &decisions,
            &calls,
        ));
    }
    findings.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.convention.cmp(&right.convention))
            .then(left.description.cmp(&right.description))
    });
    findings.dedup_by(|left, right| {
        left.file == right.file && left.convention == right.convention && left.kind == right.kind
    });
    findings
}

fn find_rule_findings(
    rule: &PolicyFlowRule,
    definitions: &[Located<'_, AggregateDefinitionFact>],
    projections: &[Located<'_, AggregateProjectionFact>],
    decisions: &[Located<'_, DecisionBranchFact>],
    calls: &[Located<'_, MethodCallFact>],
) -> Vec<Finding> {
    if rule.id.is_empty()
        || rule.source_type_id.is_empty()
        || rule.authoritative_method_id.is_empty()
        || rule.policy_fields.is_empty()
    {
        return Vec::new();
    }

    let Some(source) = definitions.iter().find(|item| {
        item.fact.type_id == rule.source_type_id
            && rule.policy_fields.iter().all(|policy_field| {
                item.fact
                    .fields
                    .iter()
                    .any(|field| field.name == *policy_field)
            })
    }) else {
        return Vec::new();
    };

    let mut findings = Vec::new();
    for projection in projections
        .iter()
        .filter(|item| item.fact.source_type_id == rule.source_type_id)
    {
        if projection.fact.field_mappings.is_empty() {
            continue;
        }
        let omitted = rule
            .policy_fields
            .iter()
            .filter(|policy_field| {
                !projection
                    .fact
                    .field_mappings
                    .iter()
                    .any(|mapping| mapping.source_field == **policy_field)
            })
            .map(String::as_str)
            .collect::<Vec<_>>();
        if omitted.is_empty() {
            continue;
        }

        for sink in rule
            .decision_sinks
            .iter()
            .filter(|sink| sink.carrier_type_id == projection.fact.target_type_id)
        {
            let Some(decision) = decisions.iter().find(|item| {
                item.fact.callable_id == sink.callable_id
                    && item.fact.domain_type_id == sink.domain_type_id
            }) else {
                continue;
            };
            if calls.iter().any(|item| {
                item.fact.caller_id == sink.callable_id
                    && item.fact.target_method_id == rule.authoritative_method_id
                    && item.fact.result_used_as_decision
                    && item.fact.decision_domain_type_id.as_deref()
                        == Some(sink.domain_type_id.as_str())
                    && item
                        .fact
                        .receiver_type_id
                        .as_deref()
                        .is_none_or(|receiver| receiver == rule.source_type_id)
            }) {
                continue;
            }

            findings.push(Finding {
                convention: stable_convention(
                    rule,
                    projection.fact,
                    &sink.callable_id,
                    &sink.domain_type_id,
                ),
                severity: parse_severity(&rule.severity),
                file: projection.file.to_string(),
                description: format!(
                    "Lossy policy projection `{}` -> `{}` at {} in `{}` omits policy field(s) [{}] owned by `{}` at {}; downstream decision `{}` branches on `{}` at {} instead of calling authoritative method `{}`.",
                    projection.fact.source_type_id,
                    projection.fact.target_type_id,
                    display_location(projection.file, &projection.fact.location),
                    projection.fact.callable_id,
                    omitted.join(", "),
                    rule.source_type_id,
                    display_location(source.file, &source.fact.location),
                    sink.callable_id,
                    decision.fact.domain_type_id,
                    display_location(decision.file, &decision.fact.location),
                    rule.authoritative_method_id,
                ),
                suggestion: format!(
                    "Preserve [{}] in the decision carrier or delegate `{}` to `{}`.",
                    omitted.join(", "),
                    sink.callable_id,
                    rule.authoritative_method_id
                ),
                kind: AuditFinding::LossyPolicyProjection,
            });
        }
    }
    findings
}

fn display_location(file: &str, location: &homeboy_audit_contract::FactLocation) -> String {
    match (location.line, location.column) {
        (0, _) => file.to_string(),
        (line, 0) => format!("{file}:{line}"),
        (line, column) => format!("{file}:{line}:{column}"),
    }
}

fn stable_convention(
    rule: &PolicyFlowRule,
    projection: &AggregateProjectionFact,
    sink_callable: &str,
    domain_type: &str,
) -> String {
    format!(
        "{}/{}/{}/{}/{}/{}/{}",
        rule.convention,
        encode_segment(&rule.id),
        encode_segment(&projection.source_type_id),
        encode_segment(&projection.target_type_id),
        encode_segment(&projection.callable_id),
        encode_segment(sink_callable),
        encode_segment(domain_type),
    )
}

fn encode_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn parse_severity(value: &str) -> Severity {
    match value {
        "info" => Severity::Info,
        _ => Severity::Warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_audit_contract::{
        AggregateDefinitionFact, AggregateProjectionFact, DecisionBranchFact, FieldAccessFact,
        MethodCallFact, PolicyFlowConfig,
    };
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Fixture {
        config: PolicyFlowConfig,
        files: Vec<FixtureFile>,
    }

    #[derive(Deserialize)]
    struct FixtureFile {
        path: String,
        #[serde(default)]
        aggregate_definitions: Vec<AggregateDefinitionFact>,
        #[serde(default)]
        field_accesses: Vec<FieldAccessFact>,
        #[serde(default)]
        aggregate_projections: Vec<AggregateProjectionFact>,
        #[serde(default)]
        decision_branches: Vec<DecisionBranchFact>,
        #[serde(default)]
        method_calls: Vec<MethodCallFact>,
    }

    fn fixture(contents: &str) -> (PolicyFlowConfig, Vec<FileFingerprint>) {
        let fixture: Fixture = serde_json::from_str(contents).expect("valid policy-flow fixture");
        let files = fixture
            .files
            .into_iter()
            .map(|file| FileFingerprint {
                relative_path: file.path,
                aggregate_definitions: file.aggregate_definitions,
                field_accesses: file.field_accesses,
                aggregate_projections: file.aggregate_projections,
                decision_branches: file.decision_branches,
                method_calls: file.method_calls,
                ..Default::default()
            })
            .collect();
        (fixture.config, files)
    }

    #[test]
    fn reports_serialized_lossy_projection_and_divergent_decision() {
        let (config, files) = fixture(include_str!(
            "../../../../tests/fixtures/audit_policy_flow/lossy_projection.json"
        ));
        let refs = files.iter().collect::<Vec<_>>();

        let findings = run(&refs, &config);

        assert_eq!(findings.len(), 1, "expected one finding: {findings:?}");
        let finding = &findings[0];
        assert_eq!(finding.kind, AuditFinding::LossyPolicyProjection);
        assert_eq!(finding.file, "src/project.ext");
        for evidence in [
            "domain::SourcePolicy",
            "domain::DecisionCarrier",
            "src/policy.ext:3:1",
            "src/project.ext:8:5",
            "src/decide.ext:21:5",
            "threshold",
            "domain::SourcePolicy::should_engage",
        ] {
            assert!(
                finding.description.contains(evidence),
                "missing evidence {evidence}: {}",
                finding.description
            );
        }
        assert_eq!(
            finding.convention,
            "policy_flow/engagement/domain%3A%3ASourcePolicy/domain%3A%3ADecisionCarrier/domain%3A%3Aproject/domain%3A%3Adecide/domain%3A%3ASeverity"
        );
    }

    #[test]
    fn authoritative_delegation_is_clean() {
        let (config, files) = fixture(include_str!(
            "../../../../tests/fixtures/audit_policy_flow/clean_delegation.json"
        ));
        let refs = files.iter().collect::<Vec<_>>();

        assert!(run(&refs, &config).is_empty());
    }

    #[test]
    fn discarded_authoritative_call_does_not_hide_divergence() {
        let (config, mut files) = fixture(include_str!(
            "../../../../tests/fixtures/audit_policy_flow/clean_delegation.json"
        ));
        files[2].method_calls[0].result_used_as_decision = false;
        let refs = files.iter().collect::<Vec<_>>();

        assert_eq!(run(&refs, &config).len(), 1);
    }

    #[test]
    fn distinct_decision_domains_have_distinct_findings() {
        let (mut config, mut files) = fixture(include_str!(
            "../../../../tests/fixtures/audit_policy_flow/lossy_projection.json"
        ));
        config.rules[0]
            .decision_sinks
            .push(homeboy_audit_contract::PolicyDecisionSink {
                carrier_type_id: "domain::DecisionCarrier".to_string(),
                callable_id: "domain::decide".to_string(),
                domain_type_id: "domain::Pressure".to_string(),
            });
        files[2].decision_branches.push(DecisionBranchFact {
            callable_id: "domain::decide".to_string(),
            domain_type_id: "domain::Pressure".to_string(),
            discriminant_id: "pressure".to_string(),
            location: homeboy_audit_contract::FactLocation {
                line: 25,
                column: 5,
            },
        });
        let refs = files.iter().collect::<Vec<_>>();

        let findings = run(&refs, &config);
        assert_eq!(findings.len(), 2);
        assert_ne!(findings[0].convention, findings[1].convention);
    }

    #[test]
    fn delegation_only_suppresses_its_resolved_decision_domain() {
        let (mut config, mut files) = fixture(include_str!(
            "../../../../tests/fixtures/audit_policy_flow/clean_delegation.json"
        ));
        config.rules[0]
            .decision_sinks
            .push(homeboy_audit_contract::PolicyDecisionSink {
                carrier_type_id: "domain::DecisionCarrier".to_string(),
                callable_id: "domain::decide".to_string(),
                domain_type_id: "domain::Pressure".to_string(),
            });
        files[2].decision_branches.push(DecisionBranchFact {
            callable_id: "domain::decide".to_string(),
            domain_type_id: "domain::Pressure".to_string(),
            discriminant_id: "pressure".to_string(),
            location: homeboy_audit_contract::FactLocation {
                line: 25,
                column: 5,
            },
        });
        let refs = files.iter().collect::<Vec<_>>();

        let findings = run(&refs, &config);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("domain::Pressure"));
    }

    #[test]
    fn ordinary_dto_projection_is_clean() {
        let (config, files) = fixture(include_str!(
            "../../../../tests/fixtures/audit_policy_flow/ordinary_dto.json"
        ));
        let refs = files.iter().collect::<Vec<_>>();

        assert!(run(&refs, &config).is_empty());
    }

    #[test]
    fn preserving_policy_field_is_clean() {
        let (config, mut files) = fixture(include_str!(
            "../../../../tests/fixtures/audit_policy_flow/lossy_projection.json"
        ));
        files[1].aggregate_projections[0].field_mappings.push(
            homeboy_audit_contract::ProjectionFieldFact {
                source_field: "threshold".to_string(),
                target_field: "threshold".to_string(),
            },
        );
        let refs = files.iter().collect::<Vec<_>>();

        assert!(run(&refs, &config).is_empty());
    }

    #[test]
    fn missing_projection_fact_disables_the_finding() {
        let (config, mut files) = fixture(include_str!(
            "../../../../tests/fixtures/audit_policy_flow/lossy_projection.json"
        ));
        files[1].aggregate_projections.clear();
        let refs = files.iter().collect::<Vec<_>>();

        assert!(run(&refs, &config).is_empty());
    }
}
