use serde::Serialize;

use crate::project::Project;
use crate::server::execute_local_command_in_dir;
use homeboy_engine_primitives::template;

use crate::extension_store::load_extension;
use homeboy_extension_contract::ExtensionManifest;

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionReadyStatus {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Environment sentinel set while a `ready_check` runs. A `ready_check` command
/// can invoke `homeboy` (e.g. `homeboy component show <id>`), which would in turn
/// re-evaluate readiness and re-run the same `ready_check` — an unbounded
/// process-spawn recursion (issue #8115). The sentinel lets a nested invocation
/// detect that it is already inside a `ready_check` and skip re-running it.
const READY_CHECK_ACTIVE_ENV: &str = "HOMEBOY_EXTENSION_READY_CHECK_ACTIVE";

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

    // Re-entry guard: if we are already inside a `ready_check`, do not run it
    // again. A `ready_check` that shells back into `homeboy` (component/extension
    // inspection) would otherwise recurse without bound. Report ready so the
    // nested inspection completes instead of spawning another check. (#8115)
    if std::env::var_os(READY_CHECK_ACTIVE_ENV).is_some() {
        return ExtensionReadyStatus {
            ready: true,
            reason: Some("ready_check_reentrant_skipped".to_string()),
            detail: Some(
                "ready_check skipped: already evaluating readiness for this extension (re-entrant invocation)".to_string(),
            ),
        };
    }

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
    // Mark the child (and anything it spawns) as running inside a ready_check so
    // a re-entrant `homeboy` invocation trips the guard above.
    let output = execute_local_command_in_dir(
        &command,
        Some(extension_path),
        Some(&[(READY_CHECK_ACTIVE_ENV, "1")]),
    );

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
            if !crate::project::has_component(project, component) {
                return false;
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with_ready_check(extension_path: &str, ready_check: &str) -> ExtensionManifest {
        let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "fixture",
            "version": "1.0.0",
            "executable": {
                "runtime": {
                    "ready_check": ready_check
                }
            }
        }))
        .expect("manifest parses");
        manifest.id = "fixture".to_string();
        manifest.extension_path = Some(extension_path.to_string());
        manifest
    }

    #[test]
    fn reentrant_ready_check_is_skipped_without_running_the_command() {
        // A ready_check that would fail if executed proves the command is never
        // run when the re-entry sentinel is already set. (#8115)
        let manifest = manifest_with_ready_check("/tmp", "exit 1");
        let prior = std::env::var_os(READY_CHECK_ACTIVE_ENV);
        std::env::set_var(READY_CHECK_ACTIVE_ENV, "1");

        let status = extension_ready_status(&manifest);

        match prior {
            Some(value) => std::env::set_var(READY_CHECK_ACTIVE_ENV, value),
            None => std::env::remove_var(READY_CHECK_ACTIVE_ENV),
        }

        assert!(
            status.ready,
            "a re-entrant ready_check must report ready instead of recursing"
        );
        assert_eq!(
            status.reason.as_deref(),
            Some("ready_check_reentrant_skipped")
        );
    }

    #[test]
    fn ready_check_runs_when_not_reentrant() {
        let manifest = manifest_with_ready_check("/tmp", "true");
        let prior = std::env::var_os(READY_CHECK_ACTIVE_ENV);
        std::env::remove_var(READY_CHECK_ACTIVE_ENV);

        let status = extension_ready_status(&manifest);

        match prior {
            Some(value) => std::env::set_var(READY_CHECK_ACTIVE_ENV, value),
            None => std::env::remove_var(READY_CHECK_ACTIVE_ENV),
        }

        assert!(status.ready, "a passing ready_check reports ready");
        assert_eq!(status.reason, None);
    }
}
