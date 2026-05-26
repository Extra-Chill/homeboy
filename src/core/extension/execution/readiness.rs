use serde::Serialize;

use crate::core::engine::template;
use crate::core::project::Project;
use crate::core::server::execute_local_command_in_dir;

use super::{load_extension, ExtensionManifest};

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionReadyStatus {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub fn extension_ready_status(extension: &ExtensionManifest) -> ExtensionReadyStatus {
    let Some(runtime) = extension.runtime() else {
        return ExtensionReadyStatus {
            ready: true,
            reason: None,
            detail: None,
        };
    };

    let Some(ready_check) = runtime.ready_check.as_ref() else {
        return ExtensionReadyStatus {
            ready: true,
            reason: None,
            detail: None,
        };
    };

    let Some(extension_path) = extension.extension_path.as_ref() else {
        return ExtensionReadyStatus {
            ready: false,
            reason: Some("missing_extension_path".to_string()),
            detail: Some("ready_check configured but extension_path is missing".to_string()),
        };
    };

    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let vars: Vec<(&str, &str)> = vec![
        ("extension_path", extension_path.as_str()),
        ("entrypoint", entrypoint.as_str()),
    ];
    let command = template::render(ready_check, &vars);
    let output = execute_local_command_in_dir(&command, Some(extension_path), None);

    if output.success {
        return ExtensionReadyStatus {
            ready: true,
            reason: None,
            detail: None,
        };
    }

    let detail_output = if output.stderr.trim().is_empty() {
        output.stdout
    } else {
        output.stderr
    };
    let detail = detail_output.trim();
    let detail = if detail.is_empty() {
        format!(
            "ready_check '{}' failed with exit code {}",
            command, output.exit_code
        )
    } else {
        format!(
            "ready_check '{}' failed with exit code {}: {}",
            command, output.exit_code, detail
        )
    };

    ExtensionReadyStatus {
        ready: false,
        reason: Some("ready_check_failed".to_string()),
        detail: Some(detail),
    }
}

/// Check if a extension is compatible with a project.
pub fn is_extension_compatible(extension: &ExtensionManifest, project: Option<&Project>) -> bool {
    let Some(ref requires) = extension.requires else {
        return true;
    };

    // Required extensions must be installed globally
    for required_extension in &requires.extensions {
        if load_extension(required_extension).is_err() {
            return false;
        }
    }

    // Required components must be linked to the project (if project context exists)
    if let Some(project) = project {
        for component in &requires.components {
            if !crate::core::project::has_component(project, component) {
                return false;
            }
        }
    }

    true
}
