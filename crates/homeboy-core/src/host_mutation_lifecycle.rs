use serde::{Deserialize, Serialize};

use crate::{Error, Result};

pub const HOST_MUTATION_LIFECYCLE_SCHEMA: &str = "homeboy/host-mutation-lifecycle/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostMutationLifecycle {
    #[serde(default = "host_mutation_lifecycle_schema")]
    pub schema: String,
    pub owner: String,
    pub run_id: String,
    #[serde(default)]
    pub mutations: Vec<HostMutationRecord>,
}

impl HostMutationLifecycle {
    pub fn validate(&self) -> Result<()> {
        validate_host_mutation_lifecycle(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostMutationRecord {
    pub id: String,
    pub actor: String,
    #[serde(flatten)]
    pub mutation: HostMutation,
    pub status: HostMutationStatus,
    pub revert: HostMutationRevertPlan,
}

impl HostMutationRecord {
    pub fn validate(&self, index: usize) -> Result<()> {
        validate_host_mutation_record(index, self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostMutation {
    Symlink {
        link_path: String,
        target_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_target_path: Option<String>,
    },
    TempDir {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        purpose: Option<String>,
    },
    FileBackup {
        original_path: String,
        backup_path: String,
    },
    PackageManifestRewrite {
        manifest_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package_manager: Option<String>,
        changes: Vec<PackageManifestChange>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageManifestChange {
    pub package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostMutationStatus {
    Declared,
    Applied,
    RevertPending,
    Reverted,
    Retained,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostMutationRevertPlan {
    pub strategy: HostMutationRevertStrategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostMutationRevertStrategy {
    RemovePath,
    RestoreBackup,
    RestoreSymlink,
    RestorePackageManifest,
    Manual,
}

pub fn validate_host_mutation_lifecycle(contract: &HostMutationLifecycle) -> Result<()> {
    if contract.schema != HOST_MUTATION_LIFECYCLE_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "expected {HOST_MUTATION_LIFECYCLE_SCHEMA}, received {}",
                contract.schema
            ),
            Some(contract.schema.clone()),
            None,
        ));
    }

    validate_required_field("owner", &contract.owner)?;
    validate_required_field("run_id", &contract.run_id)?;

    for (index, mutation) in contract.mutations.iter().enumerate() {
        validate_host_mutation_record(index, mutation)?;
    }

    Ok(())
}

pub fn validate_host_mutation_record(index: usize, record: &HostMutationRecord) -> Result<()> {
    let prefix = format!("mutations[{index}]");
    validate_required_field(&format!("{prefix}.id"), &record.id)?;
    validate_required_field(&format!("{prefix}.actor"), &record.actor)?;
    validate_host_mutation(&prefix, &record.mutation)?;
    validate_revert_plan(&prefix, &record.mutation, &record.revert)
}

fn validate_host_mutation(prefix: &str, mutation: &HostMutation) -> Result<()> {
    match mutation {
        HostMutation::Symlink {
            link_path,
            target_path,
            previous_target_path,
        } => {
            validate_required_field(&format!("{prefix}.link_path"), link_path)?;
            validate_required_field(&format!("{prefix}.target_path"), target_path)?;
            validate_optional_field(
                &format!("{prefix}.previous_target_path"),
                previous_target_path,
            )
        }
        HostMutation::TempDir { path, purpose } => {
            validate_required_field(&format!("{prefix}.path"), path)?;
            validate_optional_field(&format!("{prefix}.purpose"), purpose)
        }
        HostMutation::FileBackup {
            original_path,
            backup_path,
        } => {
            validate_required_field(&format!("{prefix}.original_path"), original_path)?;
            validate_required_field(&format!("{prefix}.backup_path"), backup_path)
        }
        HostMutation::PackageManifestRewrite {
            manifest_path,
            package_manager,
            changes,
        } => {
            validate_required_field(&format!("{prefix}.manifest_path"), manifest_path)?;
            validate_optional_field(&format!("{prefix}.package_manager"), package_manager)?;
            if changes.is_empty() {
                return Err(Error::validation_invalid_argument(
                    format!("{prefix}.changes"),
                    "package manifest rewrites require at least one package change",
                    None,
                    None,
                ));
            }
            for (change_index, change) in changes.iter().enumerate() {
                let change_prefix = format!("{prefix}.changes[{change_index}]");
                validate_required_field(&format!("{change_prefix}.package"), &change.package)?;
                validate_optional_field(&format!("{change_prefix}.before"), &change.before)?;
                validate_optional_field(&format!("{change_prefix}.after"), &change.after)?;
                if change.before.is_none() && change.after.is_none() {
                    return Err(Error::validation_invalid_argument(
                        format!("{change_prefix}.before"),
                        "package manifest change requires before or after value",
                        None,
                        None,
                    ));
                }
            }
            Ok(())
        }
    }
}

fn validate_revert_plan(
    prefix: &str,
    mutation: &HostMutation,
    revert: &HostMutationRevertPlan,
) -> Result<()> {
    validate_optional_field(&format!("{prefix}.revert.notes"), &revert.notes)?;
    validate_optional_field(&format!("{prefix}.revert.backup_path"), &revert.backup_path)?;
    validate_optional_field(&format!("{prefix}.revert.target_path"), &revert.target_path)?;

    let valid = match mutation {
        HostMutation::Symlink {
            previous_target_path,
            ..
        } => match revert.strategy {
            HostMutationRevertStrategy::RemovePath => true,
            HostMutationRevertStrategy::RestoreSymlink => previous_target_path.is_some(),
            HostMutationRevertStrategy::Manual => revert.notes.is_some(),
            _ => false,
        },
        HostMutation::TempDir { .. } => match revert.strategy {
            HostMutationRevertStrategy::RemovePath => true,
            HostMutationRevertStrategy::Manual => revert.notes.is_some(),
            _ => false,
        },
        HostMutation::FileBackup { .. } => match revert.strategy {
            HostMutationRevertStrategy::RestoreBackup => revert.backup_path.is_some(),
            HostMutationRevertStrategy::Manual => revert.notes.is_some(),
            _ => false,
        },
        HostMutation::PackageManifestRewrite { .. } => match revert.strategy {
            HostMutationRevertStrategy::RestorePackageManifest => revert.backup_path.is_some(),
            HostMutationRevertStrategy::Manual => revert.notes.is_some(),
            _ => false,
        },
    };

    if valid {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        format!("{prefix}.revert.strategy"),
        "revert strategy must be compatible with the mutation kind and include required restore metadata",
        None,
        None,
    ))
}

fn validate_required_field(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            field,
            "must not be blank",
            None,
            None,
        ));
    }

    Ok(())
}

fn validate_optional_field(field: &str, value: &Option<String>) -> Result<()> {
    if value
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(Error::validation_invalid_argument(
            field,
            "must not be blank when present",
            None,
            None,
        ));
    }

    Ok(())
}

fn host_mutation_lifecycle_schema() -> String {
    HOST_MUTATION_LIFECYCLE_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    fn revert(strategy: HostMutationRevertStrategy) -> HostMutationRevertPlan {
        HostMutationRevertPlan {
            strategy,
            backup_path: None,
            target_path: None,
            notes: None,
        }
    }

    fn contract(mutation: HostMutation, revert: HostMutationRevertPlan) -> HostMutationLifecycle {
        HostMutationLifecycle {
            schema: HOST_MUTATION_LIFECYCLE_SCHEMA.to_string(),
            owner: "homeboy-test".to_string(),
            run_id: "run-1".to_string(),
            mutations: vec![HostMutationRecord {
                id: "mutation-1".to_string(),
                actor: "test".to_string(),
                mutation,
                status: HostMutationStatus::Applied,
                revert,
            }],
        }
    }

    #[test]
    fn validates_symlink_mutation_with_remove_revert() {
        contract(
            HostMutation::Symlink {
                link_path: "/tmp/current".to_string(),
                target_path: "/tmp/releases/new".to_string(),
                previous_target_path: None,
            },
            revert(HostMutationRevertStrategy::RemovePath),
        )
        .validate()
        .unwrap();
    }

    #[test]
    fn validates_package_manifest_rewrite_with_backup_restore() {
        let mut restore = revert(HostMutationRevertStrategy::RestorePackageManifest);
        restore.backup_path = Some("package.json.homeboy-backup".to_string());

        contract(
            HostMutation::PackageManifestRewrite {
                manifest_path: "package.json".to_string(),
                package_manager: Some("npm".to_string()),
                changes: vec![PackageManifestChange {
                    package: "example-package".to_string(),
                    before: Some("^1.0.0".to_string()),
                    after: Some("^1.1.0".to_string()),
                }],
            },
            restore,
        )
        .validate()
        .unwrap();
    }

    #[test]
    fn rejects_wrong_schema() {
        let mut contract = contract(
            HostMutation::TempDir {
                path: "/tmp/homeboy-run".to_string(),
                purpose: None,
            },
            revert(HostMutationRevertStrategy::RemovePath),
        );
        contract.schema = "homeboy/host-mutation-lifecycle/v0".to_string();

        let error = contract.validate().unwrap_err();

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.details["field"], "schema");
    }

    #[test]
    fn rejects_blank_mutation_path() {
        let error = contract(
            HostMutation::FileBackup {
                original_path: "  ".to_string(),
                backup_path: "file.homeboy-backup".to_string(),
            },
            HostMutationRevertPlan {
                strategy: HostMutationRevertStrategy::RestoreBackup,
                backup_path: Some("file.homeboy-backup".to_string()),
                target_path: None,
                notes: None,
            },
        )
        .validate()
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.details["field"], "mutations[0].original_path");
    }

    #[test]
    fn rejects_incompatible_revert_strategy() {
        let error = contract(
            HostMutation::TempDir {
                path: "/tmp/homeboy-run".to_string(),
                purpose: None,
            },
            revert(HostMutationRevertStrategy::RestoreBackup),
        )
        .validate()
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.details["field"], "mutations[0].revert.strategy");
    }

    #[test]
    fn rejects_empty_manifest_rewrite_changes() {
        let mut restore = revert(HostMutationRevertStrategy::RestorePackageManifest);
        restore.backup_path = Some("package.json.homeboy-backup".to_string());

        let error = contract(
            HostMutation::PackageManifestRewrite {
                manifest_path: "package.json".to_string(),
                package_manager: None,
                changes: vec![],
            },
            restore,
        )
        .validate()
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.details["field"], "mutations[0].changes");
    }
}
