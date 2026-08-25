use serde::Serialize;

use super::execution::{
    extension_ready_status_with, is_extension_compatible, ExtensionReadinessMode,
    ExtensionReadinessState,
};
use super::manifest::ActionType;
use super::{evaluate_core_compatibility, CoreCompatibilityReport};
use homeboy_core::extension_store::{
    discover_extensions, is_extension_linked, DiscoveredExtension, ExtensionManifestFailure,
};
use homeboy_extension_contract::NotificationTransportDescriptor;

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
    pub readiness: ExtensionReadinessState,
    pub ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_cache_age_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_probe_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_follow_up_command: Option<String>,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notification_transports: Vec<NotificationTransportDescriptor>,
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
/// Aggregates cached readiness, compatibility, linked status, CLI info,
/// actions, and runtime details into a single summary per extension.
pub fn list_summaries(project: Option<&homeboy_core::project::Project>) -> Vec<ExtensionSummary> {
    list_summaries_with(project, ExtensionReadinessMode::Cached)
}

/// [`list_summaries`], with control over whether readiness is actually probed.
///
/// Every field except live readiness is read from installed metadata and costs
/// nothing. Callers that need a fresh answer must explicitly request `Probe`.
pub fn list_summaries_with(
    project: Option<&homeboy_core::project::Project>,
    readiness: ExtensionReadinessMode,
) -> Vec<ExtensionSummary> {
    let extensions = discover_extensions();

    let mut summaries: Vec<ExtensionSummary> = if readiness == ExtensionReadinessMode::Probe {
        std::thread::scope(|scope| {
            let probes = extensions
                .into_iter()
                .map(|extension| {
                    scope.spawn(move || summary_for_extension(extension, project, readiness))
                })
                .collect::<Vec<_>>();
            probes
                .into_iter()
                .map(|probe| probe.join().expect("extension readiness probe panicked"))
                .collect()
        })
    } else {
        extensions
            .into_iter()
            .map(|extension| summary_for_extension(extension, project, readiness))
            .collect()
    };

    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    summaries
}

fn summary_for_extension(
    extension: DiscoveredExtension,
    project: Option<&homeboy_core::project::Project>,
    readiness: ExtensionReadinessMode,
) -> ExtensionSummary {
    match extension {
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
            let notification_transports = ext
                .notification_transports
                .iter()
                .map(|transport| transport.descriptor())
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
                readiness: ready_status.state,
                ready: ready_status.ready,
                ready_reason: ready_status.reason,
                ready_detail: ready_status.detail,
                readiness_cache_age_seconds: ready_status.cache_age_seconds,
                readiness_probe_duration_ms: ready_status.probe_duration_ms,
                readiness_timeout_ms: ready_status.timeout_ms,
                readiness_follow_up_command: ready_status.follow_up_command,
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
                notification_transports,
                has_setup,
                has_ready_check,
            }
        }
        DiscoveredExtension::Invalid(failure) => invalid_summary(failure),
    }
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
        readiness: ExtensionReadinessState::NotReady,
        ready: Some(false),
        ready_reason: Some(failure.category.to_string()),
        ready_detail: Some(failure.diagnostic.to_string()),
        readiness_cache_age_seconds: None,
        readiness_probe_duration_ms: None,
        readiness_timeout_ms: None,
        readiness_follow_up_command: None,
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
        notification_transports: Vec::new(),
        has_setup: None,
        has_ready_check: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::paths;

    #[cfg(unix)]
    fn write_ready_check_extension(home: &std::path::Path, id: &str, ready_check: String) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": id,
                "version": "1.0.0",
                "executable": { "runtime": { "ready_check": ready_check } }
            })
            .to_string(),
        )
        .expect("extension manifest");
    }

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
            assert_eq!(summaries[0].ready, Some(false));
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

    #[cfg(unix)]
    #[test]
    fn live_readiness_probes_independent_extensions_concurrently() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let first = home.path().join("first-started");
            let second = home.path().join("second-started");
            let wait_for_peer = |own: &std::path::Path, peer: &std::path::Path| {
                format!(
                    "touch '{}' && i=0; while [ ! -f '{}' ] && [ $i -lt 200 ]; do i=$((i + 1)); sleep 0.01; done; test -f '{}'",
                    own.display(),
                    peer.display(),
                    peer.display()
                )
            };
            write_ready_check_extension(home.path(), "first", wait_for_peer(&first, &second));
            write_ready_check_extension(home.path(), "second", wait_for_peer(&second, &first));

            let summaries = list_summaries_with(None, ExtensionReadinessMode::Probe);

            assert_eq!(summaries.len(), 2);
            assert!(summaries
                .iter()
                .all(|summary| summary.readiness == ExtensionReadinessState::Ready));
        });
    }
}
