//! Discover a kind's workloads: static declarations, JSON-pointer probes over
//! the component, and the runner's own list-only listing.

use std::collections::BTreeSet;
use std::path::Path;

use homeboy_extension_contract::WorkloadListProbe;
use serde::{Deserialize, Serialize};

use crate::engine::run_dir::RunDir;
use crate::error::{Error, Result};
use crate::extension::invoke::{build_scenario_runner, ScenarioRunnerOptions};

use super::env;
use super::resolve::ResolvedWorkloadKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadListing {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `manifest`, `probe:json_pointer` or `probe:script`.
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Deserialize)]
struct ScriptListing {
    #[serde(default)]
    workloads: Vec<WorkloadListing>,
}

/// Resolve a JSON pointer probe against the component source.
fn json_pointer_declares(source_path: &Path, path: &str, pointer: &str) -> bool {
    let Ok(raw) = std::fs::read_to_string(source_path.join(path)) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

/// Static and JSON-pointer workloads; never executes anything.
pub fn declared_workloads(resolved: &ResolvedWorkloadKind) -> Vec<WorkloadListing> {
    let mut listings = resolved
        .config
        .workloads
        .iter()
        .map(|workload| WorkloadListing {
            id: workload.id.clone(),
            label: workload.label.clone(),
            description: workload.description.clone(),
            source: "manifest".to_string(),
        })
        .collect::<Vec<_>>();
    for probe in &resolved.config.list_probes {
        if let WorkloadListProbe::JsonPointer {
            path,
            pointer,
            id,
            label,
            description,
        } = probe
        {
            if json_pointer_declares(&resolved.context.source_path, path, pointer) {
                listings.push(WorkloadListing {
                    id: id.clone(),
                    label: label.clone(),
                    description: description.clone(),
                    source: "probe:json_pointer".to_string(),
                });
            }
        }
    }
    listings
}

/// Whether the kind declares a script probe.
pub fn has_script_probe(resolved: &ResolvedWorkloadKind) -> bool {
    resolved
        .config
        .list_probes
        .iter()
        .any(|probe| matches!(probe, WorkloadListProbe::Script))
}

/// Ask the runner for its workloads (`HOMEBOY_WORKLOAD_LIST_ONLY=1`).
pub fn script_workloads(
    resolved: &ResolvedWorkloadKind,
    settings: &[(String, String)],
    settings_json: &[(String, serde_json::Value)],
) -> Result<Vec<WorkloadListing>> {
    if resolved.extension.script_path.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            format!("workloads.{}.script", resolved.kind),
            format!(
                "extension `{}` declares a script list probe for `{}` but no script",
                resolved.extension_id(),
                resolved.kind
            ),
            None,
            None,
        ));
    }
    let run_dir = RunDir::create()?;
    let results = run_dir.step_file("workload-list.json");
    let artifact_dir = run_dir.step_file("workload-artifacts");
    std::fs::create_dir_all(&artifact_dir).map_err(|error| {
        Error::internal_io(error.to_string(), Some(artifact_dir.display().to_string()))
    })?;
    let runner = build_scenario_runner(ScenarioRunnerOptions {
        execution_context: &resolved.extension,
        component: &resolved.context.component,
        path_override: Some(resolved.context.source_path.to_string_lossy().to_string()),
        settings,
        settings_json,
        run_dir: &run_dir,
        results_env: Some((env::RESULTS_FILE, results.clone())),
        scenario_env: None,
        artifact_env: Some((env::ARTIFACT_DIR, &artifact_dir)),
        list_only_env: Some((env::LIST_ONLY, true)),
        extra_workloads_env: None,
        env_provider_extensions: &[],
        invocation_requirements: Default::default(),
    })?
    .env(env::KIND, &resolved.kind)
    .passthrough(false);
    let output = runner.run()?;
    if !output.success {
        run_dir.finish(false);
        return Err(Error::validation_invalid_argument(
            "list",
            format!(
                "`{}` list probe exited {}: {}",
                resolved.kind,
                output.exit_code,
                crate::extension::stderr_tail(&output.stderr)
            ),
            None,
            None,
        ));
    }
    let raw = std::fs::read(&results).map_err(|error| {
        Error::validation_invalid_argument(
            "list",
            format!(
                "`{}` list probe wrote no listing to {}: {error}",
                resolved.kind,
                env::RESULTS_FILE
            ),
            None,
            None,
        )
    })?;
    let listing: ScriptListing = serde_json::from_slice(&raw).map_err(|error| {
        Error::validation_invalid_argument(
            "list",
            format!(
                "`{}` list probe wrote an invalid listing: {error}",
                resolved.kind
            ),
            None,
            None,
        )
    })?;
    run_dir.finish(true);
    Ok(listing
        .workloads
        .into_iter()
        .filter(|workload| !workload.id.trim().is_empty())
        .map(|mut workload| {
            workload.source = "probe:script".to_string();
            workload
        })
        .collect())
}

/// Ids declared more than once across sources.
pub fn duplicate_ids(listings: &[WorkloadListing]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for listing in listings {
        if !seen.insert(listing.id.as_str()) {
            duplicates.insert(listing.id.clone());
        }
    }
    duplicates.into_iter().collect()
}
