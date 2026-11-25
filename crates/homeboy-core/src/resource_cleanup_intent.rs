use crate::validation::validate_required_field;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

pub const RESOURCE_CLEANUP_INTENT_SCHEMA: &str = "homeboy/resource-cleanup-intent/v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResourceCleanupIntent {
    #[default]
    DryRun,
    Apply,
}

impl ResourceCleanupIntent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DryRun => "dry_run",
            Self::Apply => "apply",
        }
    }

    pub fn is_apply(self) -> bool {
        matches!(self, Self::Apply)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceCleanupIntentContract {
    #[serde(default = "resource_cleanup_intent_schema")]
    pub schema: String,
    #[serde(default)]
    pub intent: ResourceCleanupIntent,
    pub ownership: ResourceCleanupIntentOwnership,
}

impl ResourceCleanupIntentContract {
    pub fn validate(&self) -> Result<()> {
        validate_resource_cleanup_intent_contract(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceCleanupIntentOwnership {
    pub dry_run: ResourceCleanupOwnershipMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply: Option<ResourceCleanupOwnershipMetadata>,
}

impl ResourceCleanupIntentOwnership {
    pub fn validate_for_intent(&self, intent: ResourceCleanupIntent) -> Result<()> {
        validate_resource_cleanup_ownership(self, intent)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceCleanupOwnershipMetadata {
    pub owner: String,
    pub declared_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ResourceCleanupOwnershipMetadata {
    pub fn validate(&self, field: &str) -> Result<()> {
        validate_resource_cleanup_ownership_metadata(field, self)
    }
}

pub fn validate_resource_cleanup_intent_contract(
    contract: &ResourceCleanupIntentContract,
) -> Result<()> {
    if contract.schema != RESOURCE_CLEANUP_INTENT_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "expected {RESOURCE_CLEANUP_INTENT_SCHEMA}, received {}",
                contract.schema
            ),
            Some(contract.schema.clone()),
            None,
        ));
    }

    validate_resource_cleanup_ownership(&contract.ownership, contract.intent)
}

pub fn validate_resource_cleanup_ownership(
    ownership: &ResourceCleanupIntentOwnership,
    intent: ResourceCleanupIntent,
) -> Result<()> {
    validate_resource_cleanup_ownership_metadata("ownership.dry_run", &ownership.dry_run)?;

    if let Some(apply) = &ownership.apply {
        validate_resource_cleanup_ownership_metadata("ownership.apply", apply)?;
    } else if intent.is_apply() {
        return Err(Error::validation_invalid_argument(
            "ownership.apply",
            "apply cleanup intent requires explicit apply ownership metadata",
            None,
            None,
        ));
    }

    Ok(())
}

pub fn validate_resource_cleanup_ownership_metadata(
    field: &str,
    metadata: &ResourceCleanupOwnershipMetadata,
) -> Result<()> {
    validate_required_field(&format!("{field}.owner"), &metadata.owner)?;
    validate_required_field(&format!("{field}.declared_by"), &metadata.declared_by)?;

    if let Some(reason) = &metadata.reason {
        validate_required_field(&format!("{field}.reason"), reason)?;
    }

    Ok(())
}

fn resource_cleanup_intent_schema() -> String {
    RESOURCE_CLEANUP_INTENT_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    fn metadata(owner: &str) -> ResourceCleanupOwnershipMetadata {
        ResourceCleanupOwnershipMetadata {
            owner: owner.to_string(),
            declared_by: "homeboy-test".to_string(),
            reason: Some("cleanup generated resources".to_string()),
        }
    }

    fn contract(intent: ResourceCleanupIntent) -> ResourceCleanupIntentContract {
        ResourceCleanupIntentContract {
            schema: RESOURCE_CLEANUP_INTENT_SCHEMA.to_string(),
            intent,
            ownership: ResourceCleanupIntentOwnership {
                dry_run: metadata("rig"),
                apply: None,
            },
        }
    }

    #[test]
    fn contract_requires_current_schema_and_owner_for_each_intent() {
        let mut apply = contract(ResourceCleanupIntent::Apply);
        apply.ownership.apply = Some(metadata("extension"));
        for valid in [contract(ResourceCleanupIntent::DryRun), apply] {
            valid.validate().unwrap();
        }

        let mut wrong_schema = contract(ResourceCleanupIntent::DryRun);
        wrong_schema.schema = "homeboy/resource-cleanup-intent/v0".to_string();
        let mut blank_owner = contract(ResourceCleanupIntent::DryRun);
        blank_owner.ownership.dry_run = metadata("  ");
        let mut blank_apply_owner = contract(ResourceCleanupIntent::Apply);
        blank_apply_owner.ownership.apply = Some(metadata(" "));

        for (invalid, field) in [
            (contract(ResourceCleanupIntent::Apply), "ownership.apply"),
            (wrong_schema, "schema"),
            (blank_owner, "ownership.dry_run.owner"),
            (blank_apply_owner, "ownership.apply.owner"),
        ] {
            let error = invalid.validate().unwrap_err();
            assert_eq!(error.code, ErrorCode::ValidationInvalidArgument, "{field}");
            assert_eq!(error.details["field"], field);
        }
    }
}
