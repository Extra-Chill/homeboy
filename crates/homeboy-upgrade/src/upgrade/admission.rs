//! Controller-upgrade admission hook.

use homeboy_core::error::Error;
use homeboy_core::error::Result;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ControllerUpgradeAdmission {
    pub schema: &'static str,
    pub active: usize,
    pub stale: usize,
    pub suspect: usize,
    pub unreconciled: usize,
    pub reconcilable: usize,
    pub record_health: serde_json::Value,
    pub blockers: Vec<ControllerUpgradeBlocker>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ControllerUpgradeBlocker {
    pub run_id: String,
    /// State plane that exclusively owns remediation for this blocker.
    pub owner: String,
    /// Bounded state affected by the owning reconciler.
    pub scope: String,
    /// State the owning action establishes when it succeeds.
    pub postcondition: String,
    pub liveness: &'static str,
    pub reason: String,
    /// The single executable action selected for this blocker.
    pub action: String,
    /// Compatibility alias for installer error remediation lists.
    pub recovery_command: String,
}

/// Immutable installer facts reported when a staged candidate cannot replace
/// the installed controller.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifiedTargetUpgrade {
    pub installed_identity: String,
    pub candidate_identity: String,
    pub selected_tag_or_artifact: Option<String>,
    pub operation_class: &'static str,
}

impl VerifiedTargetUpgrade {
    pub fn classify(
        installed_identity: String,
        candidate_identity: String,
        selected_tag_or_artifact: Option<String>,
    ) -> Self {
        let installed_version = identity_version(&installed_identity);
        let candidate_version = identity_version(&candidate_identity);
        let installed_display = normalized_identity(&installed_identity);
        let candidate_display = normalized_identity(&candidate_identity);
        let operation_class = match (installed_version, candidate_version) {
            (Some(installed), Some(candidate)) if candidate > installed => "version_upgrade",
            (Some(installed), Some(candidate)) if candidate < installed => "version_downgrade",
            (Some(_), Some(_)) if installed_display != candidate_display => {
                "source_build_replacement"
            }
            (Some(_), Some(_)) => "exact_version_repair",
            _ => "unverified_replacement",
        };
        Self {
            installed_identity,
            candidate_identity,
            selected_tag_or_artifact,
            operation_class,
        }
    }
}

fn identity_version(identity: &str) -> Option<semver::Version> {
    let identity = normalized_identity(identity);
    identity
        .trim()
        .strip_prefix("homeboy ")?
        .trim_start_matches('v')
        .split('+')
        .next()
        .filter(|version| !version.is_empty())
        .and_then(|version| semver::Version::parse(version).ok())
}

fn normalized_identity(identity: &str) -> String {
    let json_identity = serde_json::from_str::<serde_json::Value>(identity)
        .ok()
        .and_then(|identity| {
            identity
                .get("display")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    json_identity.unwrap_or_else(|| identity.trim().to_string())
}

impl ControllerUpgradeAdmission {
    pub fn allows_controller_replacement(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// Supplies agent-task liveness evidence before the controller binary changes.
pub trait ControllerUpgradeAdmissionProvider: Send + Sync {
    fn controller_upgrade_admission(&self) -> Result<ControllerUpgradeAdmission>;

    /// Repair only ownership that the target controller can prove is stale
    /// fixture provenance, then re-evaluate admission. This is invoked only by
    /// a checksum- and version-verified staged release candidate.
    fn recover_controller_upgrade_admission_for_verified_target(
        &self,
    ) -> Result<ControllerUpgradeAdmission> {
        self.controller_upgrade_admission()
    }
}

struct NoopControllerUpgradeAdmissionProvider;

impl ControllerUpgradeAdmissionProvider for NoopControllerUpgradeAdmissionProvider {
    fn controller_upgrade_admission(&self) -> Result<ControllerUpgradeAdmission> {
        Ok(ControllerUpgradeAdmission {
            schema: "homeboy/controller-upgrade-admission/v1",
            active: 0,
            stale: 0,
            suspect: 0,
            unreconciled: 0,
            reconcilable: 0,
            record_health: serde_json::Value::Null,
            blockers: Vec::new(),
        })
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn ControllerUpgradeAdmissionProvider,
    noop: NoopControllerUpgradeAdmissionProvider,
    register: pub fn register_controller_upgrade_admission_provider,
    with: pub(crate) fn with_controller_upgrade_admission,
}

/// Run the admission provider from a candidate controller before an installer
/// promotes it over the controller that invoked the upgrade.
pub fn controller_upgrade_admission() -> Result<ControllerUpgradeAdmission> {
    with_controller_upgrade_admission(|provider| provider.controller_upgrade_admission())
}

pub fn ensure_controller_upgrade_admission() -> Result<ControllerUpgradeAdmission> {
    ensure_admission(controller_upgrade_admission()?, None)
}

/// Admit a verified target release after its bounded ownership recovery. The
/// target version is included in errors so operators can distinguish this from
/// a same-version repair attempt on the installed controller.
pub fn ensure_verified_target_upgrade_admission(
    target: &VerifiedTargetUpgrade,
) -> Result<ControllerUpgradeAdmission> {
    let admission = with_controller_upgrade_admission(|provider| {
        provider.recover_controller_upgrade_admission_for_verified_target()
    })?;
    ensure_admission(admission, Some(target))
}

fn ensure_admission(
    admission: ControllerUpgradeAdmission,
    target: Option<&VerifiedTargetUpgrade>,
) -> Result<ControllerUpgradeAdmission> {
    if admission.allows_controller_replacement() {
        return Ok(admission);
    }

    let commands = admission
        .blockers
        .iter()
        .map(|blocker| blocker.recovery_command.clone())
        .collect::<Vec<_>>();
    let mut error = Error::validation_invalid_argument(
        "controller_upgrade",
        format!(
            "refusing {} while durable orchestration ownership is live or unverified: {}",
            target
                .map(|target| format!(
                    "{} (installed: {}; candidate: {}; selected tag/artifact: {})",
                    target.operation_class,
                    target.installed_identity,
                    target.candidate_identity,
                    target
                        .selected_tag_or_artifact
                        .as_deref()
                        .unwrap_or("unavailable"),
                ))
                .unwrap_or_else(|| "same-version controller repair".to_string()),
            admission
                .blockers
                .iter()
                .map(|blocker| blocker.run_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None,
        Some(commands),
    );
    error.details["controller_upgrade_admission"] = serde_json::to_value(&admission)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    if let Some(target) = target {
        error.details["verified_target_upgrade"] = serde_json::to_value(target)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_target_classifies_version_upgrade() {
        let target = VerifiedTargetUpgrade::classify(
            "homeboy 0.367.1+02c50568c814".to_string(),
            "homeboy 0.367.3+4eb24ce80".to_string(),
            Some("v0.367.3".to_string()),
        );

        assert_eq!(target.operation_class, "version_upgrade");
        assert_eq!(target.installed_identity, "homeboy 0.367.1+02c50568c814");
        assert_eq!(target.candidate_identity, "homeboy 0.367.3+4eb24ce80");
        assert_eq!(target.selected_tag_or_artifact.as_deref(), Some("v0.367.3"));
    }

    #[test]
    fn verified_target_classifies_json_identity_from_installed_controller() {
        let target = VerifiedTargetUpgrade::classify(
            r#"{"active_binary":"/usr/local/bin/homeboy","version":"0.367.1","display":"homeboy 0.367.1+02c50568c814"}"#.to_string(),
            "homeboy 0.367.3+4eb24ce80".to_string(),
            Some("v0.367.3".to_string()),
        );

        assert_eq!(target.operation_class, "version_upgrade");
    }

    #[test]
    fn verified_target_classifies_matching_json_identity_as_exact_repair() {
        let target = VerifiedTargetUpgrade::classify(
            r#"{"display":"homeboy 0.367.3+4eb24ce80"}"#.to_string(),
            "homeboy 0.367.3+4eb24ce80".to_string(),
            Some("v0.367.3".to_string()),
        );

        assert_eq!(target.operation_class, "exact_version_repair");
    }

    #[test]
    fn verified_target_classifies_exact_version_repair() {
        let target = VerifiedTargetUpgrade::classify(
            "homeboy 0.367.3+4eb24ce80".to_string(),
            "homeboy 0.367.3+4eb24ce80".to_string(),
            Some("v0.367.3".to_string()),
        );

        assert_eq!(target.operation_class, "exact_version_repair");
        assert_eq!(target.installed_identity, target.candidate_identity);
    }

    #[test]
    fn verified_target_classifies_equal_version_source_replacement() {
        let target = VerifiedTargetUpgrade::classify(
            "homeboy 0.367.3+02c50568c814".to_string(),
            "homeboy 0.367.3+4eb24ce80".to_string(),
            None,
        );

        assert_eq!(target.operation_class, "source_build_replacement");
    }

    #[test]
    fn verified_target_classifies_version_downgrade() {
        let target = VerifiedTargetUpgrade::classify(
            "homeboy 0.367.3+4eb24ce80".to_string(),
            "homeboy 0.367.1+02c50568c814".to_string(),
            Some("v0.367.1".to_string()),
        );

        assert_eq!(target.operation_class, "version_downgrade");
    }

    #[test]
    fn staged_candidate_blocker_preserves_admission_diagnostics() {
        let admission = ControllerUpgradeAdmission {
            schema: "homeboy/controller-upgrade-admission/v1",
            active: 1,
            stale: 0,
            suspect: 0,
            unreconciled: 0,
            reconcilable: 0,
            record_health: serde_json::Value::Null,
            blockers: vec![ControllerUpgradeBlocker {
                run_id: "run-1".to_string(),
                owner: "agent-task".to_string(),
                scope: "run-1".to_string(),
                postcondition: "terminal".to_string(),
                liveness: "live",
                reason: "active".to_string(),
                action: "homeboy agent-task reconcile run-1".to_string(),
                recovery_command: "homeboy agent-task reconcile run-1".to_string(),
            }],
        };
        let target = VerifiedTargetUpgrade::classify(
            "homeboy 0.367.1+02c50568c814".to_string(),
            "homeboy 0.367.3+4eb24ce80".to_string(),
            Some("v0.367.3".to_string()),
        );

        let error =
            ensure_admission(admission, Some(&target)).expect_err("staged candidate blocks");

        assert!(error.message.contains("version_upgrade"));
        assert!(error.message.contains("0.367.1+02c50568c814"));
        assert!(error.message.contains("0.367.3+4eb24ce80"));
        assert!(error.message.contains("v0.367.3"));
        assert_eq!(
            error.details["verified_target_upgrade"]["operation_class"],
            "version_upgrade"
        );
    }
}
