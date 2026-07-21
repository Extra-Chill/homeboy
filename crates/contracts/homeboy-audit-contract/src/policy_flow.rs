//! Language-neutral policy-flow facts and detector declarations.

use serde::{Deserialize, Serialize};

/// A source location within the file that emitted the containing fact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactLocation {
    /// One-based source line, or zero when unavailable.
    #[serde(default)]
    pub line: usize,
    /// One-based source column, or zero when unavailable.
    #[serde(default)]
    pub column: usize,
}

/// A field declared by an aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateFieldFact {
    pub name: String,
    /// Canonical field type identity when the extension can resolve it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_id: Option<String>,
}

/// A resolved aggregate definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateDefinitionFact {
    /// Canonical, module-qualified aggregate identity.
    pub type_id: String,
    #[serde(default)]
    pub fields: Vec<AggregateFieldFact>,
    #[serde(default)]
    pub location: FactLocation,
}

/// Whether a field access reads or writes the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldAccessKind {
    Read,
    Write,
}

/// A resolved field read or write within a callable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldAccessFact {
    pub owner_type_id: String,
    pub field: String,
    /// Canonical, module-qualified enclosing function or method identity.
    pub callable_id: String,
    pub access: FieldAccessKind,
    #[serde(default)]
    pub location: FactLocation,
}

/// One field copied by an aggregate projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionFieldFact {
    pub source_field: String,
    pub target_field: String,
}

/// A transformation from one resolved aggregate type into another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateProjectionFact {
    pub source_type_id: String,
    pub target_type_id: String,
    /// Canonical identity of the function or method performing the projection.
    pub callable_id: String,
    #[serde(default)]
    pub field_mappings: Vec<ProjectionFieldFact>,
    #[serde(default)]
    pub location: FactLocation,
}

/// A typed match/branch discriminant used by a decision-bearing callable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionBranchFact {
    pub callable_id: String,
    /// Canonical type identity of the value being classified.
    pub domain_type_id: String,
    /// Stable extension-rendered discriminant identity, not source text.
    pub discriminant_id: String,
    #[serde(default)]
    pub location: FactLocation,
}

/// A resolved method/function call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodCallFact {
    pub caller_id: String,
    pub target_method_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_type_id: Option<String>,
    /// Whether the call result directly governs the caller's decision.
    #[serde(default)]
    pub result_used_as_decision: bool,
    /// Canonical decision-domain type governed by the result, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_domain_type_id: Option<String>,
    #[serde(default)]
    pub location: FactLocation,
}

/// A downstream decision seam governed by one policy owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecisionSink {
    pub carrier_type_id: String,
    pub callable_id: String,
    pub domain_type_id: String,
}

/// Project- or extension-owned semantic declaration for one policy flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyFlowRule {
    pub id: String,
    pub source_type_id: String,
    #[serde(default)]
    pub policy_fields: Vec<String>,
    pub authoritative_method_id: String,
    #[serde(default)]
    pub decision_sinks: Vec<PolicyDecisionSink>,
    #[serde(default = "default_convention")]
    pub convention: String,
    #[serde(
        default = "default_severity",
        deserialize_with = "deserialize_severity"
    )]
    pub severity: String,
}

fn default_convention() -> String {
    "policy_flow".to_string()
}

fn default_severity() -> String {
    "warning".to_string()
}

fn deserialize_severity<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    match value.as_str() {
        "warning" | "info" => Ok(value),
        _ => Err(serde::de::Error::custom(
            "policy-flow severity must be `warning` or `info`",
        )),
    }
}

/// Declarations for the generic policy-flow detector.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyFlowConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<PolicyFlowRule>,
}

impl PolicyFlowConfig {
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub(crate) fn merge(&mut self, other: &Self) {
        for rule in &other.rules {
            if !self.rules.iter().any(|existing| existing.id == rule.id) {
                self.rules.push(rule.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuditConfig, FingerprintOutput};

    #[test]
    fn fingerprint_output_accepts_policy_flow_facts() {
        let output: FingerprintOutput = serde_json::from_value(serde_json::json!({
            "aggregate_definitions": [{
                "type_id": "domain::Policy",
                "fields": [{"name": "threshold", "type_id": "domain::Threshold"}],
                "location": {"line": 4, "column": 2}
            }],
            "field_accesses": [{
                "owner_type_id": "domain::Policy",
                "field": "threshold",
                "callable_id": "domain::Policy::allows",
                "access": "read",
                "location": {"line": 8, "column": 3}
            }],
            "aggregate_projections": [{
                "source_type_id": "domain::Policy",
                "target_type_id": "domain::Carrier",
                "callable_id": "domain::project",
                "field_mappings": [{"source_field": "id", "target_field": "id"}],
                "location": {"line": 12, "column": 1}
            }],
            "decision_branches": [{
                "callable_id": "domain::decide",
                "domain_type_id": "domain::Severity",
                "discriminant_id": "severity",
                "location": {"line": 20, "column": 1}
            }],
            "method_calls": [{
                "caller_id": "domain::decide",
                "target_method_id": "domain::Policy::allows",
                "receiver_type_id": "domain::Policy",
                "result_used_as_decision": true,
                "decision_domain_type_id": "domain::Severity",
                "location": {"line": 21, "column": 1}
            }]
        }))
        .expect("policy-flow facts deserialize");

        assert_eq!(output.aggregate_definitions[0].location.line, 4);
        assert_eq!(output.field_accesses[0].access, FieldAccessKind::Read);
        assert_eq!(output.aggregate_projections[0].field_mappings.len(), 1);
        assert_eq!(
            output.decision_branches[0].domain_type_id,
            "domain::Severity"
        );
        assert_eq!(
            output.method_calls[0].target_method_id,
            "domain::Policy::allows"
        );
        assert!(output.method_calls[0].result_used_as_decision);
        assert_eq!(
            output.method_calls[0].decision_domain_type_id.as_deref(),
            Some("domain::Severity")
        );
    }

    #[test]
    fn older_fingerprint_output_defaults_policy_flow_facts() {
        let output: FingerprintOutput = serde_json::from_str("{}").unwrap();

        assert!(output.aggregate_definitions.is_empty());
        assert!(output.field_accesses.is_empty());
        assert!(output.aggregate_projections.is_empty());
        assert!(output.decision_branches.is_empty());
        assert!(output.method_calls.is_empty());
    }

    #[test]
    fn declaration_defaults_and_merge_are_stable() {
        let first: PolicyFlowConfig = serde_json::from_value(serde_json::json!({
            "rules": [{
                "id": "policy",
                "source_type_id": "domain::Policy",
                "policy_fields": ["threshold"],
                "authoritative_method_id": "domain::Policy::allows",
                "decision_sinks": []
            }]
        }))
        .unwrap();
        assert_eq!(first.rules[0].convention, "policy_flow");
        assert_eq!(first.rules[0].severity, "warning");

        let mut merged = first.clone();
        merged.merge(&PolicyFlowConfig {
            rules: vec![first.rules[0].clone()],
        });
        assert_eq!(merged.rules.len(), 1);

        let mut audit = AuditConfig {
            policy_flow: first,
            ..Default::default()
        };
        assert!(!audit.is_empty());
        audit.merge(&AuditConfig {
            policy_flow: merged,
            ..Default::default()
        });
        assert_eq!(audit.policy_flow.rules.len(), 1);
    }

    #[test]
    fn declaration_rejects_unknown_severity() {
        let result = serde_json::from_value::<PolicyFlowConfig>(serde_json::json!({
            "rules": [{
                "id": "policy",
                "source_type_id": "domain::Policy",
                "policy_fields": ["threshold"],
                "authoritative_method_id": "domain::Policy::allows",
                "decision_sinks": [],
                "severity": "warn"
            }]
        }));

        assert!(result.is_err());
    }
}
