use serde::Serialize;

use crate::error::{Error, ErrorCode, Result};

use super::version::VersionConstraint;

pub const CORE_INCOMPATIBLE_DIAGNOSTIC: &str = "homeboy_core.incompatible";
pub const CORE_COMPAT_REMEDIATION_COMMAND: &str = "homeboy upgrade";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CoreCompatibilityReport {
    pub status: String,
    pub installed_homeboy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_homeboy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation_command: Option<String>,
}

impl CoreCompatibilityReport {
    pub fn undeclared(source_revision: Option<String>) -> Self {
        Self {
            status: "undeclared".to_string(),
            installed_homeboy: installed_homeboy_version(),
            requires_homeboy: None,
            source_revision,
            remediation_command: None,
        }
    }
}

pub fn installed_homeboy_version() -> String {
    homeboy_product_identity::product_version().to_string()
}

pub fn evaluate_core_compatibility(
    requires_homeboy: Option<&str>,
    source_revision: Option<String>,
) -> Result<CoreCompatibilityReport> {
    let installed = installed_homeboy_version();
    let Some(constraint) = requires_homeboy.filter(|value| !value.trim().is_empty()) else {
        return Ok(CoreCompatibilityReport::undeclared(source_revision));
    };

    let parsed_constraint = VersionConstraint::parse(constraint)?;
    let installed_version = semver::Version::parse(&installed).map_err(|err| {
        Error::validation_invalid_argument(
            "homeboy_version",
            format!(
                "Installed homeboy version '{}' is not valid semver: {}",
                installed, err
            ),
            Some(installed.clone()),
            None,
        )
    })?;
    let compatible = parsed_constraint.matches(&installed_version);

    Ok(CoreCompatibilityReport {
        status: if compatible {
            "compatible".to_string()
        } else {
            "incompatible".to_string()
        },
        installed_homeboy: installed,
        requires_homeboy: Some(constraint.to_string()),
        source_revision,
        remediation_command: (!compatible).then(|| CORE_COMPAT_REMEDIATION_COMMAND.to_string()),
    })
}

pub fn validate_core_compatibility(
    subject_kind: &str,
    subject_id: &str,
    requires_homeboy: Option<&str>,
    source_revision: Option<String>,
) -> Result<()> {
    let report = evaluate_core_compatibility(requires_homeboy, source_revision)?;
    if report.status == "incompatible" {
        return Err(core_incompatible_error(subject_kind, subject_id, report));
    }
    Ok(())
}

pub fn core_incompatible_error(
    subject_kind: &str,
    subject_id: &str,
    report: CoreCompatibilityReport,
) -> Error {
    let constraint = report.requires_homeboy.as_deref().unwrap_or("<undeclared>");
    let source_revision = report.source_revision.as_deref().unwrap_or("<missing>");
    let remediation = report
        .remediation_command
        .as_deref()
        .unwrap_or(CORE_COMPAT_REMEDIATION_COMMAND);
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "Invalid argument 'homeboy_core': {subject_kind} '{subject_id}' requires homeboy {constraint}, but installed homeboy is {}. Run `{remediation}` and retry.",
            report.installed_homeboy
        ),
        serde_json::json!({
            "field": "homeboy_core",
            "problem": CORE_INCOMPATIBLE_DIAGNOSTIC,
            "diagnostic": {
                "code": CORE_INCOMPATIBLE_DIAGNOSTIC,
                "subject_kind": subject_kind,
                "subject_id": subject_id,
                "installed_homeboy": report.installed_homeboy,
                "requires_homeboy": constraint,
                "source_revision": source_revision,
                "remediation_command": remediation,
            },
            "tried": [
                format!("Installed homeboy version: {}", report.installed_homeboy),
                format!("Declared homeboy constraint: {constraint}"),
                format!("Source revision: {source_revision}"),
                format!("Remediation: {remediation}"),
            ]
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undeclared_constraint_is_allowed() {
        let report = evaluate_core_compatibility(None, None).expect("compat report");
        assert_eq!(report.status, "undeclared");
        assert!(report.requires_homeboy.is_none());
    }

    #[test]
    fn satisfied_constraint_is_compatible() {
        let current = installed_homeboy_version();
        let report = evaluate_core_compatibility(Some(&format!(">={current}")), None)
            .expect("compat report");
        assert_eq!(report.status, "compatible");
        assert_eq!(
            report.requires_homeboy.as_deref(),
            Some(format!(">={current}").as_str())
        );
    }

    #[test]
    fn unsatisfied_constraint_returns_typed_error_with_remediation() {
        let err = validate_core_compatibility(
            "extension",
            "example",
            Some(">=999.0.0"),
            Some("abc123".to_string()),
        )
        .expect_err("incompatible core should fail");

        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(
            err.details["diagnostic"]["code"],
            CORE_INCOMPATIBLE_DIAGNOSTIC
        );
        assert_eq!(
            err.details["diagnostic"]["installed_homeboy"],
            installed_homeboy_version()
        );
        assert_eq!(err.details["diagnostic"]["requires_homeboy"], ">=999.0.0");
        assert_eq!(err.details["diagnostic"]["source_revision"], "abc123");
        assert_eq!(
            err.details["diagnostic"]["remediation_command"],
            CORE_COMPAT_REMEDIATION_COMMAND
        );
        assert!(err.message.contains("homeboy upgrade"));
    }
}
