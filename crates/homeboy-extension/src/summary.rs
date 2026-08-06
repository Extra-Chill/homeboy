use serde::Serialize;

use super::execution::{
    extension_ready_status_with, is_extension_compatible, ExtensionReadinessMode,
};
use super::manifest::ActionType;
use super::{evaluate_core_compatibility, CoreCompatibilityReport};
use homeboy_core::extension_store::{
    discover_extensions, is_extension_linked, DiscoveredExtension, ExtensionManifestFailure,
};

/// Summary of an extension for list views.
#[derive(Debug, Clone, Serialize)]
pub struct ExtensionSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub runtime: String,
    pub compatible: bool,
    pub core_compatibility: CoreCompatibilityReport,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_detail: Option<String>,
    pub linked: bool,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_display_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_setup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_ready_check: Option<bool>,
}

/// Summary of an extension action.
#[derive(Debug, Clone, Serialize)]
pub struct ActionSummary {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub action_type: ActionType,
}

/// List all extensions with pre-computed summary fields.
///
/// Aggregates ready status, compatibility, linked status, CLI info, actions,
/// and runtime details into a single summary per extension.
pub fn list_summaries(project: Option<&homeboy_core::project::Project>) -> Vec<ExtensionSummary> {
    list_summaries_with(project, ExtensionReadinessMode::Probe)
}

/// [`list_summaries`], with control over whether readiness is actually probed.
///
/// Every field except `ready`/`ready_reason`/`ready_detail` is read from the
/// installed manifest and costs nothing. Readiness is the only part that spawns
/// an operator-authored shell command, so it is the only part a caller should
/// ever have to opt out of (#10517).
pub fn list_summaries_with(
    project: Option<&homeboy_core::project::Project>,
    readiness: ExtensionReadinessMode,
) -> Vec<ExtensionSummary> {
    let extensions = discover_extensions();

    let mut summaries: Vec<ExtensionSummary> = extensions
        .into_iter()
        .map(|extension| match extension {
            DiscoveredExtension::Valid(ext) => {
                let ready_status = extension_ready_status_with(&ext, readiness);
                let compatible = is_extension_compatible(&ext, project);
                let linked = is_extension_linked(&ext.id);

                let (cli_tool, cli_display_name) = ext
                    .cli
                    .as_ref()
                    .map(|cli| (Some(cli.tool.clone()), Some(cli.display_name.clone())))
                    .unwrap_or((None, None));

                let actions: Vec<ActionSummary> = ext
                    .actions
                    .iter()
                    .map(|a| ActionSummary {
                        id: a.id.clone(),
                        label: a.label.clone(),
                        action_type: a.action_type.clone(),
                    })
                    .collect();

                let has_setup = ext
                    .runtime()
                    .and_then(|r| r.setup_command.as_ref())
                    .map(|_| true);
                let has_ready_check = ext
                    .runtime()
                    .and_then(|r| r.ready_check.as_ref())
                    .map(|_| true);

                let source_revision =
                    homeboy_core::extension_update_check::read_source_revision(&ext.id);
                let core_compatibility = evaluate_core_compatibility(
                    ext.requires
                        .as_ref()
                        .and_then(|requires| requires.homeboy.as_deref()),
                    source_revision.clone(),
                )
                .unwrap_or_else(|_| CoreCompatibilityReport::undeclared(source_revision.clone()));

                ExtensionSummary {
                    id: ext.id.clone(),
                    name: ext.name.clone(),
                    version: ext.version.clone(),
                    description: ext
                        .description
                        .as_ref()
                        .and_then(|d| d.lines().next())
                        .unwrap_or("")
                        .to_string(),
                    runtime: if ext.executable.is_some() {
                        "executable".to_string()
                    } else {
                        "platform".to_string()
                    },
                    compatible,
                    core_compatibility,
                    ready: ready_status.ready,
                    ready_reason: ready_status.reason,
                    ready_detail: ready_status.detail,
                    linked,
                    path: ext.extension_path.clone().unwrap_or_default(),
                    manifest_path: None,
                    error: None,
                    diagnostic: None,
                    symlink_target: None,
                    source_revision,
                    cli_tool,
                    cli_display_name,
                    actions,
                    has_setup,
                    has_ready_check,
                }
            }
            DiscoveredExtension::Invalid(failure) => invalid_summary(failure),
        })
        .collect();

    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    summaries
}

fn invalid_summary(failure: ExtensionManifestFailure) -> ExtensionSummary {
    let symlink_target = failure
        .symlink_target
        .as_ref()
        .map(|target| target.to_string_lossy().to_string());
    ExtensionSummary {
        id: failure.id,
        name: String::new(),
        version: String::new(),
        description: String::new(),
        runtime: String::new(),
        compatible: false,
        core_compatibility: CoreCompatibilityReport::undeclared(None),
        ready: false,
        ready_reason: Some(failure.category.to_string()),
        ready_detail: Some(failure.diagnostic.to_string()),
        linked: failure.path.is_symlink(),
        path: failure.path.to_string_lossy().to_string(),
        manifest_path: Some(failure.manifest_path.to_string_lossy().to_string()),
        error: Some(failure.category.to_string()),
        diagnostic: Some(failure.diagnostic.to_string()),
        symlink_target,
        source_revision: None,
        cli_tool: None,
        cli_display_name: None,
        actions: Vec::new(),
        has_setup: None,
        has_ready_check: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::paths;

    #[cfg(unix)]
    #[test]
    fn list_summaries_includes_broken_extension_symlinks() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let extensions_dir = paths::extensions().unwrap();
            std::fs::create_dir_all(&extensions_dir).unwrap();
            let link = extensions_dir.join("sample-runtime");
            let target = extensions_dir.join("missing-sample-runtime");
            std::os::unix::fs::symlink(&target, &link).unwrap();

            let summaries = list_summaries(None);

            assert_eq!(summaries.len(), 1);
            assert_eq!(summaries[0].id, "sample-runtime");
            assert!(!summaries[0].ready);
            assert!(summaries[0].linked);
            assert_eq!(summaries[0].error.as_deref(), Some("target_missing"));
            assert_eq!(summaries[0].ready_reason.as_deref(), Some("target_missing"));
            assert_eq!(
                summaries[0].diagnostic.as_deref(),
                Some("The linked extension target does not exist.")
            );
            assert_eq!(
                summaries[0].symlink_target.as_deref(),
                Some(target.to_string_lossy().as_ref())
            );
        });
    }

    #[test]
    fn list_summaries_includes_invalid_manifests_alongside_valid_extensions() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let extensions_dir = paths::extensions().unwrap();
            let valid_dir = extensions_dir.join("valid");
            let invalid_dir = extensions_dir.join("invalid");
            std::fs::create_dir_all(&valid_dir).unwrap();
            std::fs::create_dir_all(&invalid_dir).unwrap();
            std::fs::write(
                valid_dir.join("valid.json"),
                r#"{"name":"Valid","version":"1.0.0"}"#,
            )
            .unwrap();
            std::fs::write(invalid_dir.join("invalid.json"), "{").unwrap();

            let summaries = list_summaries(None);

            assert_eq!(summaries.len(), 2);
            assert_eq!(summaries[0].id, "invalid");
            assert_eq!(
                summaries[0].error.as_deref(),
                Some("manifest_json_malformed")
            );
            assert!(summaries[0]
                .manifest_path
                .as_deref()
                .is_some_and(|path| path.ends_with("invalid/invalid.json")));
            assert_eq!(summaries[1].id, "valid");
            assert_eq!(summaries[1].name, "Valid");
        });
    }
}
