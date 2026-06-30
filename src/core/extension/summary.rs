use serde::Serialize;

use super::execution::{extension_ready_status, is_extension_compatible};
use super::lifecycle::read_source_revision;
use super::manifest::ActionType;
use super::registry::{broken_extension_links, is_extension_linked, load_all_extensions};

/// Summary of an extension for list views.
#[derive(Debug, Clone, Serialize)]
pub struct ExtensionSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub runtime: String,
    pub compatible: bool,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_detail: Option<String>,
    pub linked: bool,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
pub fn list_summaries(project: Option<&crate::core::project::Project>) -> Vec<ExtensionSummary> {
    let extensions = load_all_extensions().unwrap_or_default();

    let mut summaries: Vec<ExtensionSummary> = extensions
        .iter()
        .map(|ext| {
            let ready_status = extension_ready_status(ext);
            let compatible = is_extension_compatible(ext, project);
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

            let source_revision = read_source_revision(&ext.id);

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
                ready: ready_status.ready,
                ready_reason: ready_status.reason,
                ready_detail: ready_status.detail,
                linked,
                path: ext.extension_path.clone().unwrap_or_default(),
                error: None,
                symlink_target: None,
                source_revision,
                cli_tool,
                cli_display_name,
                actions,
                has_setup,
                has_ready_check,
            }
        })
        .collect();

    summaries.extend(broken_extension_links().into_iter().map(|link| {
        let target = link.target.to_string_lossy().to_string();
        ExtensionSummary {
            id: link.id,
            name: String::new(),
            version: String::new(),
            description: String::new(),
            runtime: String::new(),
            compatible: false,
            ready: false,
            ready_reason: Some("target_missing".to_string()),
            ready_detail: Some(format!("Linked target does not exist: {}", target)),
            linked: true,
            path: link.path.to_string_lossy().to_string(),
            error: Some("target_missing".to_string()),
            symlink_target: Some(target),
            source_revision: None,
            cli_tool: None,
            cli_display_name: None,
            actions: Vec::new(),
            has_setup: None,
            has_ready_check: None,
        }
    }));

    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    summaries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::paths;

    #[cfg(unix)]
    #[test]
    fn list_summaries_includes_broken_extension_symlinks() {
        crate::test_support::with_isolated_home(|_| {
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
                summaries[0].symlink_target.as_deref(),
                Some(target.to_string_lossy().as_ref())
            );
        });
    }
}
