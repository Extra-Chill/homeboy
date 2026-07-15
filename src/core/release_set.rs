//! Generic, caller-supplied release-set contract and preflight result.
//!
//! This module deliberately describes only membership and immutable source
//! identity. Product-specific release and deployment policy remains with callers.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const RELEASE_SET_SCHEMA: &str = "homeboy/release-set/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseSetManifest {
    pub schema: String,
    pub components: Vec<ReleaseSetComponent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseSetComponent {
    pub id: String,
    #[serde(rename = "ref")]
    pub requested_ref: String,
    #[serde(default = "required_by_default")]
    pub required: bool,
}

fn required_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NormalizedReleaseSet {
    pub schema: String,
    pub identity: String,
    pub components: Vec<ReleaseSetComponent>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReleaseSetObservation {
    pub id: String,
    pub resolved_ref: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReleaseSetComparison {
    pub identity: String,
    pub missing: Vec<String>,
    pub unexpected: Vec<String>,
    pub mismatched: Vec<ReleaseSetMismatch>,
    pub matched: Vec<String>,
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReleaseSetMismatch {
    pub id: String,
    pub expected_ref: String,
    pub observed_ref: String,
}

impl ReleaseSetManifest {
    pub fn parse_json(input: &str) -> Result<NormalizedReleaseSet, String> {
        let manifest: Self = serde_json::from_str(input).map_err(|error| error.to_string())?;
        manifest.normalize()
    }

    pub fn normalize(self) -> Result<NormalizedReleaseSet, String> {
        if self.schema != RELEASE_SET_SCHEMA {
            return Err(format!("release set schema must be {RELEASE_SET_SCHEMA}"));
        }
        if self.components.is_empty() {
            return Err("release set must declare at least one component".to_string());
        }
        let mut ids = BTreeSet::new();
        for component in &self.components {
            if component.id.trim().is_empty() || component.requested_ref.trim().is_empty() {
                return Err("release set component id and ref must not be empty".to_string());
            }
            if !ids.insert(component.id.clone()) {
                return Err(format!(
                    "release set contains duplicate component id '{}'",
                    component.id
                ));
            }
        }
        let mut components = self.components;
        components.sort_by(|left, right| left.id.cmp(&right.id));
        let canonical = serde_json::to_vec(&(RELEASE_SET_SCHEMA, &components))
            .map_err(|error| error.to_string())?;
        let identity = format!("sha256:{:x}", Sha256::digest(canonical));
        Ok(NormalizedReleaseSet {
            schema: RELEASE_SET_SCHEMA.to_string(),
            identity,
            components,
        })
    }
}

impl NormalizedReleaseSet {
    pub fn compare(&self, observations: &[ReleaseSetObservation]) -> ReleaseSetComparison {
        let expected = self
            .components
            .iter()
            .map(|component| (component.id.as_str(), component))
            .collect::<BTreeMap<_, _>>();
        let observed = observations
            .iter()
            .map(|component| (component.id.as_str(), component))
            .collect::<BTreeMap<_, _>>();
        let missing: Vec<String> = self
            .components
            .iter()
            .filter(|component| component.required && !observed.contains_key(component.id.as_str()))
            .map(|component| component.id.clone())
            .collect();
        let unexpected: Vec<String> = observed
            .keys()
            .filter(|id| !expected.contains_key(**id))
            .map(|id| (*id).to_string())
            .collect();
        let mismatched: Vec<ReleaseSetMismatch> = self
            .components
            .iter()
            .filter_map(|component| {
                let observed = observed.get(component.id.as_str())?;
                (component.requested_ref != observed.resolved_ref).then(|| ReleaseSetMismatch {
                    id: component.id.clone(),
                    expected_ref: component.requested_ref.clone(),
                    observed_ref: observed.resolved_ref.clone(),
                })
            })
            .collect::<Vec<_>>();
        let matched: Vec<String> = self
            .components
            .iter()
            .filter(|component| {
                observed
                    .get(component.id.as_str())
                    .is_some_and(|value| value.resolved_ref == component.requested_ref)
            })
            .map(|component| component.id.clone())
            .collect();
        let ready = missing.is_empty() && unexpected.is_empty() && mismatched.is_empty();
        ReleaseSetComparison {
            identity: self.identity.clone(),
            missing,
            unexpected,
            mismatched,
            matched,
            ready,
        }
    }

    /// Runs all validation before invoking a mutation closure. A red comparison
    /// never calls `mutate`, which makes the boundary directly testable by callers.
    pub fn mutate_if_ready<T>(
        &self,
        observations: &[ReleaseSetObservation],
        mutate: impl FnOnce() -> T,
    ) -> Result<T, ReleaseSetComparison> {
        let comparison = self.compare(observations);
        if comparison.ready {
            Ok(mutate())
        } else {
            Err(comparison)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(components: serde_json::Value) -> String {
        serde_json::json!({ "schema": RELEASE_SET_SCHEMA, "components": components }).to_string()
    }

    #[test]
    // 1. Duplicate IDs are rejected.
    fn rejects_duplicate_component_ids() {
        let error = ReleaseSetManifest::parse_json(&manifest(serde_json::json!([
            {"id":"a", "ref":"one"}, {"id":"a", "ref":"two"}
        ])))
        .expect_err("duplicate ids must fail");
        assert!(error.contains("duplicate"));
    }

    #[test]
    // 2. Identity is independent of caller component ordering.
    fn identity_is_stable_across_component_order() {
        let first = ReleaseSetManifest::parse_json(&manifest(serde_json::json!([
            {"id":"b", "ref":"two"}, {"id":"a", "ref":"one"}
        ])))
        .expect("first manifest");
        let second = ReleaseSetManifest::parse_json(&manifest(serde_json::json!([
            {"id":"a", "ref":"one"}, {"id":"b", "ref":"two"}
        ])))
        .expect("second manifest");
        assert_eq!(first.identity, second.identity);
    }

    #[test]
    fn identity_changes_when_membership_policy_changes() {
        let required = ReleaseSetManifest::parse_json(&manifest(serde_json::json!([
            {"id":"a", "ref":"one"}
        ])))
        .expect("required manifest");
        let optional = ReleaseSetManifest::parse_json(&manifest(serde_json::json!([
            {"id":"a", "ref":"one", "required":false}
        ])))
        .expect("optional manifest");

        assert_ne!(required.identity, optional.identity);
    }

    #[test]
    // 3. Comparison reports every red observation class deterministically.
    fn reports_missing_unexpected_and_mismatched_observations() {
        let set = ReleaseSetManifest::parse_json(&manifest(serde_json::json!([
            {"id":"required", "ref":"one"}, {"id":"optional", "ref":"two", "required":false},
            {"id":"wrong", "ref":"three"}
        ])))
        .expect("manifest");
        let report = set.compare(&[
            ReleaseSetObservation {
                id: "wrong".into(),
                resolved_ref: "other".into(),
            },
            ReleaseSetObservation {
                id: "extra".into(),
                resolved_ref: "four".into(),
            },
        ]);
        assert_eq!(report.missing, ["required"]);
        assert!(!report.missing.contains(&"optional".to_string()));
        assert_eq!(report.unexpected, ["extra"]);
        assert_eq!(report.mismatched[0].id, "wrong");
        assert!(!report.ready);
    }

    #[test]
    // 4. A red preflight cannot cross the mutation boundary.
    fn red_preflight_performs_zero_mutations() {
        let set = ReleaseSetManifest::parse_json(&manifest(serde_json::json!([
            {"id":"a", "ref":"one"}
        ])))
        .expect("manifest");
        let mut mutations = 0;
        let result = set.mutate_if_ready(&[], || mutations += 1);
        assert!(result.is_err());
        assert_eq!(mutations, 0);
    }
}
