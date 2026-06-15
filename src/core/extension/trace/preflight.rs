use std::collections::BTreeSet;
use std::path::Path;

use crate::core::error::{Error, Result};
use crate::core::extension::ExtensionExecutionContext;
use crate::core::rig::TraceDependencySpec;

use super::parsing::TraceDependencyProvenance;

pub(super) fn preflight_trace_dependencies(
    dependencies: &[TraceDependencySpec],
) -> Result<Vec<TraceDependencyProvenance>> {
    let provenance = dependencies
        .iter()
        .map(preflight_trace_dependency)
        .collect::<Result<Vec<_>>>()?;

    let checkouts = dependencies
        .iter()
        .filter_map(|dependency| {
            let path = dependency.path.as_deref().filter(|path| !path.is_empty())?;
            Some(crate::core::hygiene::DependencyCheckout {
                id: dependency.id.clone(),
                role: "trace_dependency".to_string(),
                path: Path::new(path).to_path_buf(),
            })
        })
        .collect::<Vec<_>>();
    crate::core::hygiene::require_checkout_hygiene(
        checkouts,
        crate::core::hygiene::DependencyHygieneOptions { allow_stale: false },
    )?;

    Ok(provenance)
}

fn preflight_trace_dependency(
    dependency: &TraceDependencySpec,
) -> Result<TraceDependencyProvenance> {
    let Some(path) = dependency.path.as_deref().filter(|path| !path.is_empty()) else {
        return Err(trace_preflight_error(
            "trace.dependencies",
            format!("trace dependency '{}' has no resolved path", dependency.id),
            serde_json::json!({
                "kind": "trace-dependency",
                "dependency_id": dependency.id,
                "failure": "missing_path"
            }),
        ));
    };
    let root = Path::new(path);
    if !root.is_dir() {
        return Err(trace_preflight_error(
            "trace.dependencies",
            format!(
                "trace dependency '{}' path does not exist or is not a directory: {}",
                dependency.id,
                root.display()
            ),
            serde_json::json!({
                "kind": "trace-dependency",
                "dependency_id": dependency.id,
                "failure": "missing_path",
                "path": root.to_string_lossy()
            }),
        ));
    }

    if let Some(plugin_file) = dependency
        .plugin_file
        .as_deref()
        .filter(|plugin_file| !plugin_file.is_empty())
    {
        let plugin_path = root.join(plugin_file);
        if !plugin_path.is_file() {
            return Err(trace_preflight_error(
                "trace.dependencies",
                format!(
                    "trace dependency '{}' is missing declared entrypoint {}",
                    dependency.id, plugin_file
                ),
                serde_json::json!({
                    "kind": "trace-dependency",
                    "dependency_id": dependency.id,
                    "failure": "missing_plugin_file",
                    "path": root.to_string_lossy(),
                    "plugin_file": plugin_file
                }),
            ));
        }
    }

    for required_path in required_trace_dependency_paths(dependency) {
        let candidate = root.join(&required_path);
        if !candidate.exists() {
            return Err(trace_preflight_error(
                "trace.dependencies",
                format!(
                    "trace dependency '{}' is missing required packaged/build artifact {}",
                    dependency.id, required_path
                ),
                serde_json::json!({
                    "kind": "trace-dependency",
                    "dependency_id": dependency.id,
                    "failure": "missing_required_path",
                    "path": root.to_string_lossy(),
                    "required_path": required_path,
                    "requires_built_assets": dependency.requires_built_assets
                }),
            ));
        }
    }

    Ok(TraceDependencyProvenance {
        id: dependency.id.clone(),
        kind: dependency.kind.clone(),
        source: dependency.source.clone(),
        path: root.to_string_lossy().to_string(),
        source_url: dependency.source_url.clone(),
        version: dependency.version.clone(),
        r#ref: dependency.r#ref.clone(),
        package_marker: dependency.package_marker.clone(),
        plugin_file: dependency.plugin_file.clone(),
    })
}

fn required_trace_dependency_paths(dependency: &TraceDependencySpec) -> Vec<String> {
    dependency.required_paths.clone()
}

pub(super) fn preflight_trace_runner_capabilities(
    execution_context: Option<&ExtensionExecutionContext>,
    required_capabilities: &[String],
) -> Result<()> {
    if required_capabilities.is_empty() {
        return Ok(());
    }

    let provided = trace_runner_capabilities(execution_context)?;
    let missing = required_capabilities
        .iter()
        .filter(|capability| !provided.contains(*capability))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    Err(trace_preflight_error(
        "trace.runner_capabilities",
        format!(
            "trace runner is missing required capabilities: {}",
            missing.join(", ")
        ),
        serde_json::json!({
            "kind": "trace-runner-capabilities",
            "failure": "missing_capabilities",
            "missing": missing,
            "required": required_capabilities,
            "provided": provided.into_iter().collect::<Vec<_>>()
        }),
    ))
}

fn trace_runner_capabilities(
    execution_context: Option<&ExtensionExecutionContext>,
) -> Result<BTreeSet<String>> {
    let mut capabilities = BTreeSet::new();
    if let Some(raw) = std::env::var_os("HOMEBOY_TRACE_RUNNER_CAPABILITIES") {
        capabilities
            .extend(std::env::split_paths(&raw).map(|path| path.to_string_lossy().to_string()));
    }
    if let Some(raw) = std::env::var_os("HOMEBOY_TRACE_RUNNER_CAPABILITIES_JSON") {
        let parsed: Vec<String> = serde_json::from_slice(raw.as_encoded_bytes()).map_err(|e| {
            Error::validation_invalid_argument(
                "HOMEBOY_TRACE_RUNNER_CAPABILITIES_JSON",
                format!("trace runner capabilities JSON is invalid: {e}"),
                None,
                None,
            )
        })?;
        capabilities.extend(parsed);
    }
    if let Some(context) = execution_context {
        let manifest = crate::core::extension::load_extension(&context.extension_id)?;
        capabilities.extend(manifest.trace_runner_capabilities().iter().cloned());
    }
    Ok(capabilities)
}

fn trace_preflight_error(field: &str, problem: String, details: serde_json::Value) -> Error {
    Error::validation_invalid_argument(field, problem, None, Some(vec![details.to_string()]))
}

#[cfg(test)]
mod tests {
    use crate::core::error::ErrorCode;

    use super::*;

    #[test]
    fn trace_dependency_preflight_accepts_packaged_dependency() {
        let temp = tempfile::tempdir().unwrap();
        let plugin_root = temp.path().join("sample-package");
        std::fs::create_dir_all(plugin_root.join("package")).unwrap();
        std::fs::create_dir_all(plugin_root.join("vendor")).unwrap();
        std::fs::write(plugin_root.join("package/entrypoint.txt"), "entry").unwrap();

        let provenance = preflight_trace_dependencies(&[TraceDependencySpec {
            id: "sample".to_string(),
            kind: "package".to_string(),
            source: "release-package-or-build-artifact".to_string(),
            path: Some(plugin_root.to_string_lossy().to_string()),
            plugin_file: Some("package/entrypoint.txt".to_string()),
            requires_built_assets: true,
            required_paths: vec!["vendor".to_string()],
            source_url: Some("https://example.com/sample.zip".to_string()),
            version: Some("10.0.0".to_string()),
            r#ref: Some("v10.0.0".to_string()),
            package_marker: Some("packaged-zip".to_string()),
        }])
        .expect("packaged plugin dependency should pass preflight");

        assert_eq!(provenance.len(), 1);
        assert_eq!(provenance[0].id, "sample");
        assert_eq!(
            provenance[0].plugin_file.as_deref(),
            Some("package/entrypoint.txt")
        );
        assert_eq!(
            provenance[0].package_marker.as_deref(),
            Some("packaged-zip")
        );
    }

    #[test]
    fn trace_dependency_preflight_rejects_raw_dependency_without_required_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let plugin_root = temp.path().join("sample-source");
        std::fs::create_dir_all(plugin_root.join("package")).unwrap();
        std::fs::write(plugin_root.join("package/entrypoint.txt"), "entry").unwrap();

        let err = preflight_trace_dependencies(&[TraceDependencySpec {
            id: "sample".to_string(),
            kind: "package".to_string(),
            source: "release-package-or-build-artifact".to_string(),
            path: Some(plugin_root.to_string_lossy().to_string()),
            plugin_file: Some("package/entrypoint.txt".to_string()),
            requires_built_assets: true,
            required_paths: vec!["vendor".to_string()],
            source_url: None,
            version: None,
            r#ref: None,
            package_marker: None,
        }])
        .expect_err("raw plugin checkout should fail dependency preflight");

        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(err.details["field"], "trace.dependencies");
        assert!(err
            .message
            .contains("missing required packaged/build artifact vendor"));
        assert!(err.details["tried"][0]
            .as_str()
            .unwrap()
            .contains("missing_required_path"));
    }

    #[test]
    fn trace_runner_capability_preflight_reports_missing_browser_probe_feature() {
        let err = preflight_trace_runner_capabilities(
            None,
            &[
                "custom-provider.recipe-run".to_string(),
                "browser-probe.capture.network".to_string(),
            ],
        )
        .expect_err("missing capabilities should fail preflight");

        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(err.details["field"], "trace.runner_capabilities");
        assert!(err.message.contains("browser-probe.capture.network"));
        assert!(err.details["tried"][0]
            .as_str()
            .unwrap()
            .contains("missing_capabilities"));
    }
}
