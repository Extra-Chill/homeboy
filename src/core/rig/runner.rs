//! Top-level rig operations: `up`, `check`, `down`, `repair`, `status`, `snapshot`.
//!
//! Each function returns a report struct that the CLI layer serializes to
//! JSON. Reports are the contract — they should be stable across minor
//! homeboy versions.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::artifact_index::{self, RigRunArtifactIndex};

use super::capabilities::evaluate_requirements;
use super::expand::{expand_resources, expand_vars};
use super::lease::acquire_active_run_lease;
use super::lint::run_package_lint;
use super::pipeline::{
    cleanup_shared_paths, run_command_step, run_pipeline, run_pipeline_check_groups,
    run_pipeline_with_settings, run_prepare_requirement_steps, PipelineOutcome,
    PipelineStepOutcome,
};
use super::service::{self, ServiceStatus};
use super::spec::{DependencyMaterializationOutputKind, RigSpec, ServiceKind, SymlinkSpec};
use super::state::{
    now_rfc3339, ComponentSnapshot, MaterializedRigState, RigState, RigStateSnapshot,
};
use crate::core::engine::command::run_in_optional;
use crate::core::error::{Error, Result};
use crate::core::observation::{NewRunRecord, ObservationStore, RunStatus};

/// Report from `rig up`.
#[derive(Debug, Clone, Serialize)]
pub struct UpReport {
    pub rig_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub pipeline: PipelineOutcome,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_index: Option<RigRunArtifactIndex>,
}

/// Report from `rig check`.
#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub rig_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub pipeline: PipelineOutcome,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_index: Option<RigRunArtifactIndex>,
}

/// Report from a bench preparation pipeline.
#[derive(Debug, Clone, Serialize)]
pub struct BenchPrepareReport {
    pub rig_id: String,
    pub pipeline: PipelineOutcome,
    pub success: bool,
}

/// Report from a fuzz preparation pipeline.
#[derive(Debug, Clone, Serialize)]
pub struct FuzzPrepareReport {
    pub rig_id: String,
    pub pipeline: PipelineOutcome,
    pub success: bool,
}

/// Report from `rig down`.
#[derive(Debug, Clone, Serialize)]
pub struct DownReport {
    pub rig_id: String,
    pub stopped: Vec<String>,
    pub pipeline: Option<PipelineOutcome>,
    pub success: bool,
}

/// Report from `rig repair`.
#[derive(Debug, Clone, Serialize)]
pub struct RepairReport {
    pub rig_id: String,
    pub resources: Vec<RepairResourceReport>,
    pub repaired: usize,
    pub unchanged: usize,
    pub blocked: usize,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepairResourceReport {
    pub kind: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_target: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Report from `rig status`.
#[derive(Debug, Clone, Serialize)]
pub struct RigStatusReport {
    pub rig_id: String,
    pub description: String,
    pub services: Vec<ServiceStatusReport>,
    pub symlinks: Vec<SymlinkStatusReport>,
    pub last_up: Option<String>,
    pub last_check: Option<String>,
    pub last_check_result: Option<String>,
    pub materialized: Option<MaterializedRigState>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatusReport {
    pub id: String,
    pub kind: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub log_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SymlinkStatusReport {
    pub link: String,
    pub expected_target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_target: Option<String>,
    pub state: SymlinkStatusState,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SymlinkStatusState {
    Ok,
    Missing,
    Drifted,
    BlockedByNonSymlink,
}

/// Materialize a rig: run the `up` pipeline, stash timestamp in state.
pub fn run_up(rig: &RigSpec) -> Result<UpReport> {
    let _lease = acquire_active_run_lease(rig, "up")?;
    let observer = RigRunObserver::start(rig, "up");

    let mut result = (|| {
        let outcome = run_pipeline(rig, "up", true)?;

        if outcome.is_success() {
            let mut state = RigState::load(&rig.id)?;
            let materialized_at = now_rfc3339();
            let snapshot = snapshot_state(rig);
            state.last_up = Some(materialized_at.clone());
            state.materialized = Some(MaterializedRigState {
                rig_id: rig.id.clone(),
                materialized_at,
                resources: expand_resources(rig),
                components: snapshot.components,
            });
            state.save(&rig.id)?;
        }

        Ok(UpReport {
            rig_id: rig.id.clone(),
            run_id: None,
            success: outcome.is_success(),
            pipeline: outcome,
            artifact_index: None,
        })
    })();

    let artifact_index = RigRunObserver::finish(
        observer.as_ref(),
        rig,
        result.as_ref().ok().map(|report| &report.pipeline),
        &result,
    );
    if let Ok(report) = result.as_mut() {
        report.run_id = observer.as_ref().map(|observer| observer.run_id.clone());
        report.artifact_index = artifact_index;
    }
    result
}

/// Run the `check` pipeline. Unlike `up`, does NOT fail-fast — reports every
/// failing check so the user can fix them all in one pass.
pub fn run_check(rig: &RigSpec) -> Result<CheckReport> {
    let observer = RigRunObserver::start(rig, "check");

    let mut result = (|| {
        let requirements = evaluate_requirements(rig);
        let package_lint = run_package_lint(rig)?;
        let check_outcome = run_pipeline(rig, "check", false)?;
        let outcome = merge_check_outcomes(vec![requirements, package_lint, check_outcome]);

        let mut state = RigState::load(&rig.id)?;
        state.last_check = Some(now_rfc3339());
        state.last_check_result =
            Some(if outcome.is_success() { "pass" } else { "fail" }.to_string());
        state.save(&rig.id)?;

        Ok(CheckReport {
            rig_id: rig.id.clone(),
            run_id: None,
            success: outcome.is_success(),
            pipeline: outcome,
            artifact_index: None,
        })
    })();

    let artifact_index = RigRunObserver::finish(
        observer.as_ref(),
        rig,
        result.as_ref().ok().map(|report| &report.pipeline),
        &result,
    );
    if let Ok(report) = result.as_mut() {
        report.run_id = observer.as_ref().map(|observer| observer.run_id.clone());
        report.artifact_index = artifact_index;
    }
    result
}

/// Run ONLY the env-independent rig package lint.
///
/// Unlike [`run_check`], this skips `evaluate_requirements` and the live
/// `check` probe pipeline (HTTP/command/file probes) entirely. It runs just the
/// `run_package_lint` walk/conflict/JSON/template-materialize checks, which
/// depend only on the package contents on disk — no component checkouts,
/// services, or network.
///
/// This is the CI-friendly entry point: in CI no component checkouts exist, so
/// `run_check`'s requirement step always fails, making it unusable as a pure
/// package linter. `run_lint` validates package structure with zero environment
/// dependencies.
///
/// Like [`run_check_groups`], it intentionally does not update `last_check`: a
/// package lint is not proof that the whole rig passed `homeboy rig check`.
pub fn run_lint(rig: &RigSpec) -> Result<CheckReport> {
    let outcome = run_package_lint(rig)?;

    Ok(CheckReport {
        rig_id: rig.id.clone(),
        run_id: None,
        success: outcome.is_success(),
        pipeline: outcome,
        artifact_index: None,
    })
}

fn merge_check_outcomes(outcomes: Vec<PipelineOutcome>) -> PipelineOutcome {
    let mut steps = Vec::new();
    let mut passed = 0;
    let mut failed = 0;
    for outcome in outcomes {
        passed += outcome.passed;
        failed += outcome.failed;
        steps.extend(outcome.steps);
    }

    PipelineOutcome {
        name: "check".to_string(),
        passed,
        failed,
        steps,
    }
}

/// Run only the grouped check-pipeline steps required by a workload command.
///
/// This intentionally does not update `last_check`: the report is a scoped
/// command preflight, not proof that the whole rig passed `homeboy rig check`.
pub fn run_check_groups(rig: &RigSpec, groups: &[String]) -> Result<CheckReport> {
    let outcome = run_pipeline_check_groups(rig, groups, false)?;

    Ok(CheckReport {
        rig_id: rig.id.clone(),
        run_id: None,
        success: outcome.is_success(),
        pipeline: outcome,
        artifact_index: None,
    })
}

/// Run rig-owned setup required before timed bench workloads.
///
/// Missing `bench_prepare` pipelines are a no-op so existing rigs keep their
/// previous bench behavior. Declared steps fail fast because workload timing
/// must not start when dependency/bootstrap preparation is incomplete.
pub fn run_bench_prepare(
    rig: &RigSpec,
    settings: &[(String, String)],
) -> Result<Option<BenchPrepareReport>> {
    let prepare_requirements = run_prepare_requirement_steps(rig, "bench_prepare", settings)?;
    if !rig.pipeline.contains_key("bench_prepare") && prepare_requirements.steps.is_empty() {
        return Ok(None);
    }

    let pipeline_outcome =
        if prepare_requirements.is_success() && rig.pipeline.contains_key("bench_prepare") {
            run_pipeline_with_settings(rig, "bench_prepare", true, settings)?
        } else {
            PipelineOutcome {
                name: "bench_prepare".to_string(),
                steps: Vec::new(),
                passed: 0,
                failed: 0,
            }
        };
    let outcome = merge_prepare_outcomes("bench_prepare", prepare_requirements, pipeline_outcome);
    Ok(Some(BenchPrepareReport {
        rig_id: rig.id.clone(),
        success: outcome.is_success(),
        pipeline: outcome,
    }))
}

/// Run rig-owned setup required before fuzz workloads.
///
/// Missing `fuzz_prepare` pipelines are a no-op so existing rigs keep their
/// previous fuzz behavior. Declared steps fail fast because fuzz runs need the
/// same prepared runtime dependencies as the workload they exercise.
pub fn run_fuzz_prepare(
    rig: &RigSpec,
    settings: &[(String, String)],
) -> Result<Option<FuzzPrepareReport>> {
    if !rig.pipeline.contains_key("fuzz_prepare")
        && rig.requirements.dependency_materialization.is_empty()
    {
        return Ok(None);
    }

    let dependency_outcome = run_dependency_materialization_steps(rig, "fuzz_prepare", settings)?;
    let pipeline_outcome =
        if dependency_outcome.is_success() && rig.pipeline.contains_key("fuzz_prepare") {
            run_pipeline_with_settings(rig, "fuzz_prepare", true, settings)?
        } else {
            PipelineOutcome {
                name: "fuzz_prepare".to_string(),
                steps: Vec::new(),
                passed: 0,
                failed: 0,
            }
        };
    let outcome = merge_prepare_outcomes("fuzz_prepare", dependency_outcome, pipeline_outcome);
    Ok(Some(FuzzPrepareReport {
        rig_id: rig.id.clone(),
        success: outcome.is_success(),
        pipeline: outcome,
    }))
}

fn run_dependency_materialization_steps(
    rig: &RigSpec,
    phase: &str,
    settings: &[(String, String)],
) -> Result<PipelineOutcome> {
    let mut steps = Vec::new();
    let mut passed = 0;
    let mut failed = 0;

    for step in super::normalize_dependency_materialization_steps(rig) {
        let label = if step.id.is_empty() {
            "dependency materialization".to_string()
        } else {
            format!("dependency materialization {}", step.id)
        };
        let result = materialize_dependency_step(rig, &step.spec, settings);
        match result {
            Ok(()) => {
                passed += 1;
                steps.push(PipelineStepOutcome {
                    kind: "dependency_materialization".to_string(),
                    label,
                    status: "pass".to_string(),
                    error: None,
                });
            }
            Err(error) => {
                failed += 1;
                steps.push(PipelineStepOutcome {
                    kind: "dependency_materialization".to_string(),
                    label,
                    status: "fail".to_string(),
                    error: Some(error.to_string()),
                });
                break;
            }
        }
    }

    Ok(PipelineOutcome {
        name: phase.to_string(),
        steps,
        passed,
        failed,
    })
}

fn materialize_dependency_step(
    rig: &RigSpec,
    step: &super::DependencyMaterializationStepSpec,
    settings: &[(String, String)],
) -> Result<()> {
    if dependency_outputs_satisfied(rig, step) {
        return Ok(());
    }

    if let Some(command) = step.command.as_deref() {
        let env: HashMap<String, String> = step
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        run_command_step(
            rig,
            command,
            dependency_step_cwd(rig, step).as_deref(),
            &env,
            settings,
        )?;
    } else if step.provider.is_some() {
        let component_id = step.component.as_deref().ok_or_else(|| {
            Error::rig_pipeline_failed(
                &rig.id,
                "dependency_materialization",
                format!(
                    "dependency materialization step '{}' declares provider but no component",
                    step.id
                ),
            )
        })?;
        let component = super::resolve_component(rig, component_id)?;
        let path = dependency_step_cwd(rig, step)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&component.local_path));
        crate::core::deps::install_for_resolved(&component, &path).map_err(|error| {
            Error::rig_pipeline_failed(
                &rig.id,
                "dependency_materialization",
                format!(
                    "dependency materialization step '{}' provider '{}' failed: {}",
                    step.id,
                    step.provider.as_deref().unwrap_or_default(),
                    error
                ),
            )
        })?;
    }

    let missing = missing_dependency_outputs(rig, step);
    if !missing.is_empty() {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "dependency_materialization",
            format!(
                "dependency materialization step '{}' did not produce required outputs: {}",
                step.id,
                missing.join("; ")
            ),
        ));
    }

    Ok(())
}

fn dependency_outputs_satisfied(
    rig: &RigSpec,
    step: &super::DependencyMaterializationStepSpec,
) -> bool {
    missing_dependency_outputs(rig, step).is_empty()
}

fn missing_dependency_outputs(
    rig: &RigSpec,
    step: &super::DependencyMaterializationStepSpec,
) -> Vec<String> {
    step.expected_outputs
        .iter()
        .filter(|output| output.required)
        .filter_map(|output| {
            let path = resolve_dependency_output_path(rig, step, &output.path);
            let ok = match output.kind {
                DependencyMaterializationOutputKind::File => path.is_file(),
                DependencyMaterializationOutputKind::Dir => path.is_dir(),
                DependencyMaterializationOutputKind::Path => path.exists(),
            };
            (!ok).then(|| {
                format!(
                    "{} does not exist: {}",
                    output_kind_label(output.kind),
                    path.display()
                )
            })
        })
        .collect()
}

fn resolve_dependency_output_path(
    rig: &RigSpec,
    step: &super::DependencyMaterializationStepSpec,
    path: &str,
) -> PathBuf {
    let expanded = expand_vars(rig, path);
    let output_path = PathBuf::from(&expanded);
    if output_path.is_absolute() {
        return output_path;
    }
    if let Some(cwd) = dependency_step_cwd(rig, step) {
        return PathBuf::from(cwd).join(output_path);
    }
    output_path
}

fn dependency_step_cwd(
    rig: &RigSpec,
    step: &super::DependencyMaterializationStepSpec,
) -> Option<String> {
    step.cwd
        .as_deref()
        .map(|cwd| expand_vars(rig, cwd))
        .or_else(|| {
            step.component
                .as_deref()
                .and_then(|component_id| super::resolve_component_path(rig, component_id).ok())
        })
}

fn output_kind_label(kind: DependencyMaterializationOutputKind) -> &'static str {
    match kind {
        DependencyMaterializationOutputKind::File => "file",
        DependencyMaterializationOutputKind::Dir => "dir",
        DependencyMaterializationOutputKind::Path => "path",
    }
}

fn merge_prepare_outcomes(
    name: &str,
    mut left: PipelineOutcome,
    right: PipelineOutcome,
) -> PipelineOutcome {
    left.steps.extend(right.steps);
    PipelineOutcome {
        name: name.to_string(),
        passed: left.passed + right.passed,
        failed: left.failed + right.failed,
        steps: left.steps,
    }
}

/// Tear down a rig. Runs the `down` pipeline if defined, then stops every
/// service the rig knows about (belt + suspenders — spec authors sometimes
/// forget to add `service stop` steps to `down`).
pub fn run_down(rig: &RigSpec) -> Result<DownReport> {
    let _lease = acquire_active_run_lease(rig, "down")?;
    let pipeline = if rig.pipeline.contains_key("down") {
        Some(run_pipeline(rig, "down", false)?)
    } else {
        None
    };

    cleanup_shared_paths(rig)?;

    let mut stopped = Vec::new();
    for service_id in rig.services.keys() {
        service::stop(rig, service_id)?;
        stopped.push(service_id.clone());
    }
    stopped.sort();

    let success = pipeline.as_ref().is_none_or(|p| p.is_success());
    let mut state = RigState::load(&rig.id)?;
    state.materialized = None;
    state.save(&rig.id)?;

    Ok(DownReport {
        rig_id: rig.id.clone(),
        stopped,
        pipeline,
        success,
    })
}

/// Repair safe declared drift without running the heavy `up` pipeline.
///
/// v1 intentionally repairs only declared symlinks. It will create missing
/// symlinks and replace drifted symlinks, but refuses to remove real
/// files/directories at the link path.
pub fn run_repair(rig: &RigSpec) -> Result<RepairReport> {
    let _lease = acquire_active_run_lease(rig, "repair")?;
    let mut resources = Vec::new();
    let mut repaired = 0;
    let mut unchanged = 0;
    let mut blocked = 0;

    for link in &rig.symlinks {
        let resource = repair_symlink(rig, link)?;
        match resource.status.as_str() {
            "repaired" => repaired += 1,
            "unchanged" => unchanged += 1,
            "blocked" => blocked += 1,
            _ => {}
        }
        resources.push(resource);
    }

    Ok(RepairReport {
        rig_id: rig.id.clone(),
        success: blocked == 0,
        resources,
        repaired,
        unchanged,
        blocked,
    })
}

fn repair_symlink(rig: &RigSpec, link: &SymlinkSpec) -> Result<RepairResourceReport> {
    let link_path = PathBuf::from(expand_vars(rig, &link.link));
    let target_path = PathBuf::from(expand_vars(rig, &link.target));
    let path = link_path.to_string_lossy().into_owned();
    let expected_target = target_path.to_string_lossy().into_owned();

    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::rig_pipeline_failed(
                &rig.id,
                "repair",
                format!("create parent of {}: {}", link_path.display(), e),
            )
        })?;
    }

    match std::fs::symlink_metadata(&link_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current = std::fs::read_link(&link_path).map_err(|e| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "repair",
                    format!("read {}: {}", link_path.display(), e),
                )
            })?;
            let previous_target = current.to_string_lossy().into_owned();
            if current == target_path {
                return Ok(RepairResourceReport {
                    status: "unchanged".to_string(),
                    ..symlink_resource(path, expected_target, Some(previous_target), None)
                });
            }

            std::fs::remove_file(&link_path).map_err(|e| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "repair",
                    format!("remove drifted symlink {}: {}", link_path.display(), e),
                )
            })?;
            create_repair_symlink(rig, &target_path, &link_path)?;
            Ok(RepairResourceReport {
                status: "repaired".to_string(),
                ..symlink_resource(path, expected_target, Some(previous_target), None)
            })
        }
        Ok(_) => Ok(RepairResourceReport {
            status: "blocked".to_string(),
            ..symlink_resource(
                path,
                expected_target,
                None,
                Some("path exists and is not a symlink; repair will not remove it".to_string()),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            create_repair_symlink(rig, &target_path, &link_path)?;
            Ok(RepairResourceReport {
                status: "repaired".to_string(),
                ..symlink_resource(path, expected_target, None, None)
            })
        }
        Err(e) => Err(Error::rig_pipeline_failed(
            &rig.id,
            "repair",
            format!("inspect {}: {}", link_path.display(), e),
        )),
    }
}

fn symlink_resource(
    path: String,
    expected_target: String,
    previous_target: Option<String>,
    error: Option<String>,
) -> RepairResourceReport {
    RepairResourceReport {
        kind: "symlink".to_string(),
        path,
        expected_target: Some(expected_target),
        previous_target,
        status: String::new(),
        error,
    }
}

fn create_repair_symlink(rig: &RigSpec, target_path: &Path, link_path: &Path) -> Result<()> {
    create_symlink(target_path, link_path).map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "repair",
            format!(
                "create {} → {}: {}",
                link_path.display(),
                target_path.display(),
                e
            ),
        )
    })
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "rig symlink repair is not supported on this platform (Unix only)",
    ))
}

/// Summarize current rig state (no mutations).
pub fn run_status(rig: &RigSpec) -> Result<RigStatusReport> {
    let state = RigState::load(&rig.id)?;
    let mut services = Vec::with_capacity(rig.services.len());

    for (id, spec) in &rig.services {
        let live = service::status(&rig.id, id)?;
        let (status_str, pid) = match live {
            ServiceStatus::Running(pid) => ("running", Some(pid)),
            ServiceStatus::Stopped => ("stopped", None),
            ServiceStatus::Stale(pid) => ("stale", Some(pid)),
        };
        let started_at = state.services.get(id).and_then(|s| s.started_at.clone());
        let log_path = service::log_path(&rig.id, id)?
            .to_string_lossy()
            .into_owned();
        services.push(ServiceStatusReport {
            id: id.clone(),
            kind: service_kind_label(spec.kind).to_string(),
            status: status_str.to_string(),
            pid,
            port: spec.port,
            log_path,
            started_at,
        });
    }
    services.sort_by(|a, b| a.id.cmp(&b.id));

    let mut symlinks = rig
        .symlinks
        .iter()
        .map(|link| symlink_status(rig, link))
        .collect::<Vec<_>>();
    symlinks.sort_by(|a, b| a.link.cmp(&b.link));

    Ok(RigStatusReport {
        rig_id: rig.id.clone(),
        description: rig.description.clone(),
        services,
        symlinks,
        last_up: state.last_up,
        last_check: state.last_check,
        last_check_result: state.last_check_result,
        materialized: state.materialized,
    })
}

fn symlink_status(rig: &RigSpec, link: &super::spec::SymlinkSpec) -> SymlinkStatusReport {
    let link_path = PathBuf::from(expand_vars(rig, &link.link));
    let target_path = PathBuf::from(expand_vars(rig, &link.target));
    let link_display = link_path.to_string_lossy().into_owned();
    let expected_target = target_path.to_string_lossy().into_owned();

    match std::fs::symlink_metadata(&link_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let actual = std::fs::read_link(&link_path).ok();
            let state = if actual.as_ref() == Some(&target_path) {
                SymlinkStatusState::Ok
            } else {
                SymlinkStatusState::Drifted
            };
            SymlinkStatusReport {
                link: link_display,
                expected_target,
                actual_target: actual.map(|path| path.to_string_lossy().into_owned()),
                state,
            }
        }
        Ok(_) => SymlinkStatusReport {
            link: link_display,
            expected_target,
            actual_target: None,
            state: SymlinkStatusState::BlockedByNonSymlink,
        },
        Err(_) => SymlinkStatusReport {
            link: link_display,
            expected_target,
            actual_target: None,
            state: SymlinkStatusState::Missing,
        },
    }
}

fn service_kind_label(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::HttpStatic => "http-static",
        ServiceKind::Command => "command",
        ServiceKind::External => "external",
    }
}

/// Capture the current state of every component in a rig.
///
/// Resolves each `ComponentSpec.path` (with `${env.X}` / `${components.X}`
/// / `~` expansion), then queries git for HEAD SHA and current branch.
/// Components whose paths aren't git repos are still included with `sha`
/// / `branch` set to `None` — bench results should still record they were
/// part of the rig at measurement time.
pub fn snapshot_state(rig: &RigSpec) -> RigStateSnapshot {
    let mut components = BTreeMap::new();
    for (id, comp) in &rig.components {
        let expanded = expand_vars(rig, &comp.path);
        let resolved = shellexpand::tilde(&expanded).into_owned();
        let (sha, branch) = head_sha_and_branch(&resolved);
        components.insert(
            id.clone(),
            ComponentSnapshot {
                path: resolved,
                declared_path: None,
                sha,
                branch,
            },
        );
    }
    RigStateSnapshot {
        rig_id: rig.id.clone(),
        captured_at: now_rfc3339(),
        components,
    }
}

/// Look up the HEAD SHA and current branch for a path on disk.
///
/// Returns `(None, None)` for paths that aren't git repos. Used by both rig
/// snapshotting and effective-path overrides so persisted snapshots always
/// describe the checkout that was actually exercised.
pub fn head_sha_and_branch(path: &str) -> (Option<String>, Option<String>) {
    let sha = run_in_optional(path, "git", &["rev-parse", "HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let branch = run_in_optional(path, "git", &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    (sha, branch)
}

struct RigRunObserver {
    store: ObservationStore,
    run_id: String,
    command: String,
}

impl RigRunObserver {
    fn start(rig: &RigSpec, command: &str) -> Option<Self> {
        let store = ObservationStore::open_initialized().ok()?;
        let cwd = std::env::current_dir().ok();
        let run = store
            .start_run(
                NewRunRecord::builder("rig")
                    .command(format!("rig.{command}"))
                    .optional_cwd_path(cwd.as_deref())
                    .current_homeboy_version()
                    .rig_id(rig.id.clone())
                    .metadata(rig_observation_metadata(rig, command, None, None, None))
                    .build(),
            )
            .ok()?;

        Some(Self {
            store,
            run_id: run.id,
            command: command.to_string(),
        })
    }

    fn finish<T>(
        observer: Option<&Self>,
        rig: &RigSpec,
        pipeline: Option<&PipelineOutcome>,
        result: &Result<T>,
    ) -> Option<RigRunArtifactIndex> {
        let observer = observer?;
        let status = match result {
            Ok(_) if pipeline.is_some_and(PipelineOutcome::is_success) => RunStatus::Pass,
            Ok(_) => RunStatus::Fail,
            Err(_) => RunStatus::Error,
        };
        let error = result.as_ref().err().map(ToString::to_string);
        let state = RigState::load(&rig.id).ok();
        let metadata =
            rig_observation_metadata(rig, &observer.command, state, pipeline, error.as_deref());
        let _ = observer
            .store
            .finish_run(&observer.run_id, status, Some(metadata));
        artifact_index::for_completed_rig_run(
            &observer.store,
            rig,
            &observer.run_id,
            status.as_str(),
            pipeline,
        )
    }
}

fn rig_observation_metadata(
    rig: &RigSpec,
    command: &str,
    state: Option<RigState>,
    pipeline: Option<&PipelineOutcome>,
    error: Option<&str>,
) -> serde_json::Value {
    let source = super::install::read_source_metadata(&rig.id);
    serde_json::json!({
        "rig_id": rig.id,
        "command": command,
        "rig_source": source.as_ref().map(|source| &source.source),
        "rig_revision": source.as_ref().and_then(|source| source.source_revision.as_deref()),
        "source": source,
        "state": state,
        "component_snapshot": snapshot_state(rig),
        "pipeline": pipeline,
        "error": error,
    })
}

#[cfg(test)]
#[path = "../../../tests/core/rig/runner_test.rs"]
mod runner_test;

#[cfg(test)]
#[path = "../../../tests/core/rig/runner_observation_test.rs"]
mod runner_observation_test;
