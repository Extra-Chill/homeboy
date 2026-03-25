//! Language-agnostic function contract representation.
//!
//! A `FunctionContract` describes what a function promises: its signature,
//! control flow branches, side effects, and dependencies. Extensions produce
//! contracts from language-specific analysis; core consumes them for test
//! generation, documentation, refactor safety verification, and more.
//!
//! This follows the same architecture as fingerprinting:
//! - Core defines the struct and the consumer interface
//! - Extensions provide `scripts/contract.sh` to extract contracts
//! - Core never knows what language it's looking at
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/820

mod find_extension_contract;
mod function_contract;
mod types;

pub use find_extension_contract::*;
pub use function_contract::*;
pub use types::*;


use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::code_audit::core_fingerprint::load_grammar_for_ext;
use crate::error::{Error, Result};
use crate::extension;

// ── Core data types ──

// ── Control flow ──

// ── Effects ──

// ── Dependencies ──

// ── File-level container ──

// ── Type definitions ──

// ── Extraction API ──

// ── Utility methods ──

impl FunctionContract {
    /// Returns true if this function can fail (returns Result or Option).
    pub fn can_fail(&self) -> bool {
        matches!(
            self.signature.return_type,
            ReturnShape::ResultType { .. } | ReturnShape::OptionType { .. }
        )
    }

    /// Returns true if this function has side effects.
    pub fn has_effects(&self) -> bool {
        !self.effects.is_empty()
    }

    /// Returns true if this function is pure (no effects, no mutation).
    pub fn is_pure(&self) -> bool {
        self.effects.is_empty()
            && self
                .signature
                .receiver
                .as_ref()
                .is_none_or(|r| !matches!(r, Receiver::MutRef))
            && !self.signature.params.iter().any(|p| p.mutable)
    }

    /// Count the number of distinct return paths.
    pub fn branch_count(&self) -> usize {
        self.branches.len()
    }

    /// Group branches by return variant (ok/err/some/none/true/false).
    pub fn branches_by_variant(&self) -> HashMap<&str, Vec<&Branch>> {
        let mut map: HashMap<&str, Vec<&Branch>> = HashMap::new();
        for branch in &self.branches {
            map.entry(branch.returns.variant.as_str())
                .or_default()
                .push(branch);
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_contract() -> FunctionContract {
        FunctionContract {
            name: "validate_write".to_string(),
            file: "src/core/engine/validate_write.rs".to_string(),
            line: 86,
            signature: Signature {
                params: vec![
                    Param {
                        name: "root".to_string(),
                        param_type: "&Path".to_string(),
                        mutable: false,
                        has_default: false,
                    },
                    Param {
                        name: "changed_files".to_string(),
                        param_type: "&[PathBuf]".to_string(),
                        mutable: false,
                        has_default: false,
                    },
                ],
                return_type: ReturnShape::ResultType {
                    ok_type: "ValidationResult".to_string(),
                    err_type: "Error".to_string(),
                },
                receiver: None,
                is_public: true,
                is_async: false,
                generics: vec![],
            },
            branches: vec![
                Branch {
                    condition: "changed_files.is_empty()".to_string(),
                    returns: ReturnValue {
                        variant: "ok".to_string(),
                        value: Some("skipped".to_string()),
                    },
                    effects: vec![],
                    line: Some(91),
                },
                Branch {
                    condition: "validation command fails".to_string(),
                    returns: ReturnValue {
                        variant: "ok".to_string(),
                        value: Some("failed".to_string()),
                    },
                    effects: vec![
                        Effect::ProcessSpawn {
                            command: Some("sh".to_string()),
                        },
                        Effect::Mutation {
                            target: "rollback".to_string(),
                        },
                    ],
                    line: Some(130),
                },
            ],
            early_returns: 2,
            effects: vec![
                Effect::ProcessSpawn {
                    command: Some("sh".to_string()),
                },
                Effect::Mutation {
                    target: "rollback".to_string(),
                },
            ],
            calls: vec![
                FunctionCall {
                    function: "resolve_validate_command".to_string(),
                    forwards: vec!["root".to_string(), "changed_files".to_string()],
                },
                FunctionCall {
                    function: "Command::new".to_string(),
                    forwards: vec![],
                },
            ],
            impl_type: None,
        }
    }

    #[test]
    fn can_fail_returns_true_for_result() {
        let c = sample_contract();
        assert!(c.can_fail());
    }

    #[test]
    fn has_effects_returns_true() {
        let c = sample_contract();
        assert!(c.has_effects());
    }

    #[test]
    fn is_pure_returns_false_with_effects() {
        let c = sample_contract();
        assert!(!c.is_pure());
    }

    #[test]
    fn is_pure_returns_true_for_pure_function() {
        let mut c = sample_contract();
        c.effects.clear();
        for b in &mut c.branches {
            b.effects.clear();
        }
        assert!(c.is_pure());
    }

    #[test]
    fn branch_count() {
        let c = sample_contract();
        assert_eq!(c.branch_count(), 2);
    }

    #[test]
    fn branches_by_variant_groups_correctly() {
        let c = sample_contract();
        let grouped = c.branches_by_variant();
        assert_eq!(grouped.get("ok").unwrap().len(), 2);
        assert!(grouped.get("err").is_none());
    }

    #[test]
    fn contract_serializes_to_json() {
        let c = sample_contract();
        let json = serde_json::to_string_pretty(&c).unwrap();
        assert!(json.contains("validate_write"));
        assert!(json.contains("result"));
        assert!(json.contains("process_spawn"));
    }

    #[test]
    fn contract_roundtrips_through_json() {
        let c = sample_contract();
        let json = serde_json::to_string(&c).unwrap();
        let deserialized: FunctionContract = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "validate_write");
        assert_eq!(deserialized.branches.len(), 2);
        assert_eq!(deserialized.effects.len(), 2);
    }
}
