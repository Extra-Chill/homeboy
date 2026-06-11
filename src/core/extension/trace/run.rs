//! Trace workflows: invoke extension runners, parse JSON, preserve artifacts.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::core::component::Component;
use crate::core::engine::baseline::BaselineFlags;
use crate::core::engine::invocation::InvocationRequirements;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, ErrorCode, Result};
use crate::core::extension::trace::baseline::TraceBaselineComparison;
use crate::core::extension::trace::preflight::{
    preflight_trace_dependencies, preflight_trace_runner_capabilities,
};
use crate::core::extension::RunnerOutput;
use crate::core::extension::{
    build_scenario_runner, resolve_execution_context, stderr_tail, ExtensionCapability,
    ExtensionExecutionContext, ScenarioRunnerOptions,
};
use crate::core::paths;
use crate::core::rig::{RigStateSnapshot, TraceDependencySpec};

use super::attach::{append_attach_observations, observe_trace_attachments, TraceAttachment};
use super::overlay::{
    acquire_trace_overlay_locks, apply_trace_overlays, cleanup_after_overlay_error,
    cleanup_trace_overlays, TraceOverlayRequest,
};

use super::canonicality::{
    evaluate_trace_canonicality, refused_trace_result, TraceCanonicalPolicy,
};
use super::generic_runner::run_generic_trace_runner;
#[cfg(test)]
use super::generic_runner::{discover_generic_trace_workloads, trace_workload_scenario_id};
use super::parsing::{
    parse_trace_list_str, parse_trace_results_file, TraceAssertion, TraceAssertionStatus,
    TraceComponentsProvenance, TraceEvidenceMetadata, TraceGitProvenance, TraceList, TraceResults,
    TraceRuntimeAssetProvenance, TraceSpanDefinition, TraceStatus, TraceToolchainProvenance,
};
use super::preview::{TracePreviewMetadata, TracePublicPreviewSession};
use super::probes::{ActiveTraceProbes, TraceProbeConfig};

#[derive(Debug, Clone)]
pub struct TraceRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub runner_inputs: TraceRunnerInputs,
    pub scenario_id: String,
    pub json_summary: bool,
    pub rig_id: Option<String>,
    pub overlays: Vec<TraceOverlayRequest>,
    pub keep_overlay: bool,
    pub span_definitions: Vec<TraceSpanDefinition>,
    pub baseline_flags: BaselineFlags,
    pub regression_threshold_percent: f64,
    pub regression_min_delta_ms: u64,
    pub canonical_policy: TraceCanonicalPolicy,
    pub checkout_provenance: Option<TraceCheckoutProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceCheckoutProvenance {
    pub source: String,
    pub path: String,
    pub requested_ref: String,
    pub resolved_sha: String,
}

#[derive(Debug, Clone)]
pub struct TraceListWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub runner_inputs: TraceRunnerInputs,
    pub rig_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TraceRunnerInputs {
    pub json_settings: Vec<(String, serde_json::Value)>,
    pub env: Vec<(String, String)>,
    pub workload_paths: Vec<PathBuf>,
    pub probes: Vec<TraceProbeConfig>,
    pub attachments: Vec<TraceAttachment>,
    pub dependencies: Vec<TraceDependencySpec>,
    pub runner_capabilities: Vec<String>,
    pub invocation_requirements: InvocationRequirements,
    pub public_preview: Option<crate::core::rig::TracePublicPreviewSpec>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub evidence: TraceEvidenceMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<TraceResults>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<TraceRunFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TraceOverlay>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<TraceBaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toolchain: Option<TraceToolchainProvenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<TraceComponentsProvenance>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TraceOverlay {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    pub path: String,
    pub component_path: String,
    pub touched_files: Vec<String>,
    pub kept: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceRunFailure {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_override: Option<String>,
    pub scenario_id: String,
    pub exit_code: i32,
    pub stderr_excerpt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipe_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_observed_homeboy_event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_succeeded: Option<bool>,
}

pub fn run_trace_workflow(
    component: &Component,
    args: TraceRunWorkflowArgs,
    run_dir: &RunDir,
    rig_state: Option<RigStateSnapshot>,
) -> Result<TraceRunWorkflowResult> {
    if component.has_script(ExtensionCapability::Trace) {
        return run_trace_workflow_with_component_script(component, args, run_dir, rig_state);
    }

    let execution_context = match resolve_execution_context(component, ExtensionCapability::Trace) {
        Ok(execution_context) => Some(execution_context),
        Err(error) if trace_is_unclaimed(&error) => None,
        Err(error) => return Err(error),
    };
    run_trace_workflow_with_context(
        execution_context.as_ref(),
        component,
        args,
        run_dir,
        rig_state,
    )
}

fn run_trace_workflow_with_component_script(
    component: &Component,
    mut args: TraceRunWorkflowArgs,
    run_dir: &RunDir,
    rig_state: Option<RigStateSnapshot>,
) -> Result<TraceRunWorkflowResult> {
    let component_path = args
        .path_override
        .clone()
        .unwrap_or_else(|| component.local_path.clone());
    let dependency_provenance = preflight_trace_dependencies(&args.runner_inputs.dependencies)?;
    preflight_trace_runner_capabilities(None, &args.runner_inputs.runner_capabilities)?;
    let canonicality = evaluate_trace_canonicality(None, component, &args)?;
    if args.canonical_policy.refuses_non_canonical() && !canonicality.is_canonical() {
        return Ok(refused_trace_result(
            args,
            canonicality.metadata(TraceCanonicalPolicy::Canonical),
        ));
    }
    let evidence = canonicality.metadata(args.canonical_policy);
    let (mut toolchain, components) = trace_provenance(None, &component_path);
    mark_non_canonical(
        &mut toolchain,
        "component script trace runs do not resolve an extension runner checkout",
    );
    let source_path = Path::new(&component_path);
    let _overlay_locks = if args.overlays.is_empty() {
        None
    } else {
        Some(acquire_trace_overlay_locks(&args.overlays, run_dir)?)
    };
    let applied_overlays = apply_trace_overlays(&args.overlays, args.keep_overlay)?;
    let preview_session = start_trace_public_preview(&mut args)?;
    let mut script_env = vec![
        (
            "HOMEBOY_TRACE_SCENARIO".to_string(),
            args.scenario_id.clone(),
        ),
        ("HOMEBOY_TRACE_LIST_ONLY".to_string(), "0".to_string()),
    ];
    script_env.extend(args.runner_inputs.env.clone());
    let script_output =
        crate::core::extension::component_script::run_component_scripts_with_run_dir(
            component,
            ExtensionCapability::Trace,
            source_path,
            run_dir,
            true,
            &script_env,
            &[],
        );
    if !args.keep_overlay {
        cleanup_trace_overlays(&applied_overlays)?
    }
    let script_output = script_output?;
    let preview = finish_trace_public_preview(preview_session, run_dir)?;
    let results_path = run_dir.step_file(run_dir::files::TRACE_RESULTS);
    let mut results = if results_path.exists() {
        let mut parsed = parse_trace_results_file(&results_path)?;
        if parsed.rig.is_none() {
            parsed.rig = rig_state;
        }
        parsed.evidence = Some(evidence.clone());
        Some(parsed)
    } else {
        None
    };
    if let Some(parsed) = results.as_mut() {
        parsed.toolchain = Some(toolchain.clone());
        parsed.components = Some(components.clone());
        parsed.dependencies = dependency_provenance;
        apply_trace_preview_metadata(parsed, preview.as_ref());
        super::spans::apply_span_definitions(parsed, &args.span_definitions);
        super::assertions::apply_temporal_assertions(parsed);
        persist_trace_results(&results_path, parsed)?;
    }
    let status = results
        .as_ref()
        .map(|r| r.status.as_str().to_string())
        .unwrap_or_else(|| {
            if script_output.success {
                "pass"
            } else {
                "error"
            }
            .to_string()
        });
    let exit_code = if script_output.success {
        if status == "pass" {
            0
        } else {
            1
        }
    } else {
        script_output.exit_code
    };
    let failure = (!script_output.success).then(|| TraceRunFailure {
        component_id: args.component_id.clone(),
        path_override: args.path_override.clone(),
        scenario_id: args.scenario_id.clone(),
        exit_code: script_output.exit_code,
        stderr_excerpt: stderr_tail(&script_output.stderr),
        current_phase: None,
        child_pid: None,
        child_command: None,
        recipe_path: recipe_path_from_args(&args),
        artifact_root: None,
        last_observed_homeboy_event: None,
        cleanup_succeeded: None,
    });

    Ok(TraceRunWorkflowResult {
        status,
        component: args.component_label,
        exit_code,
        evidence: evidence.clone(),
        results,
        failure,
        overlays: applied_overlays
            .into_iter()
            .map(|overlay| TraceOverlay {
                variant: overlay.variant,
                component_id: overlay.component_id,
                path: overlay.patch_path.to_string_lossy().to_string(),
                component_path: overlay.component_path.to_string_lossy().to_string(),
                touched_files: overlay.touched_files,
                kept: overlay.keep,
            })
            .collect(),
        baseline_comparison: None,
        hints: Some({
            let mut hints = non_canonical_evidence_hints(&evidence);
            hints.push(
            "Component scripts use the extension runner env contract without extension resolution."
                .to_string(),
            );
            hints
        }),
        toolchain: Some(toolchain),
        components: Some(components),
    })
}

fn run_trace_workflow_with_context(
    execution_context: Option<&ExtensionExecutionContext>,
    component: &Component,
    mut args: TraceRunWorkflowArgs,
    run_dir: &RunDir,
    rig_state: Option<RigStateSnapshot>,
) -> Result<TraceRunWorkflowResult> {
    let component_path = args
        .path_override
        .clone()
        .unwrap_or_else(|| component.local_path.clone());
    let dependency_provenance = preflight_trace_dependencies(&args.runner_inputs.dependencies)?;
    preflight_trace_runner_capabilities(
        execution_context,
        &args.runner_inputs.runner_capabilities,
    )?;
    let canonicality = evaluate_trace_canonicality(execution_context, component, &args)?;
    if args.canonical_policy.refuses_non_canonical() && !canonicality.is_canonical() {
        return Ok(refused_trace_result(
            args,
            canonicality.metadata(TraceCanonicalPolicy::Canonical),
        ));
    }
    let evidence = canonicality.metadata(args.canonical_policy);
    let (toolchain, components) = trace_provenance(execution_context, &component_path);
    let _overlay_locks = if args.overlays.is_empty() {
        None
    } else {
        Some(acquire_trace_overlay_locks(&args.overlays, run_dir)?)
    };
    let applied_overlays = apply_trace_overlays(&args.overlays, args.keep_overlay)?;
    let preview_session = start_trace_public_preview(&mut args)?;
    let artifact_dir = run_dir.path().join("artifacts");
    std::fs::create_dir_all(&artifact_dir).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to create trace artifact dir {}: {}",
                artifact_dir.display(),
                e
            ),
            Some("trace.artifacts.create".to_string()),
        )
    })?;
    let probe_configs = trace_probes_with_fswatch_attachments(
        &args.runner_inputs.probes,
        &args.runner_inputs.attachments,
    );
    let active_probes =
        ActiveTraceProbes::start_with_artifact_dir(&probe_configs, Some(artifact_dir.clone()))?;
    let started_at = Instant::now();
    let mut attach_observations =
        observe_trace_attachments(&args.runner_inputs.attachments, "before", started_at);
    let runner_output =
        match build_trace_runner(execution_context, component, &args, run_dir, false) {
            Ok(output) => output,
            Err(error) => {
                return cleanup_after_overlay_error(&applied_overlays, args.keep_overlay, error)
            }
        };
    let probe_events = active_probes.stop();
    attach_observations.extend(observe_trace_attachments(
        &args.runner_inputs.attachments,
        "after",
        started_at,
    ));
    if !args.keep_overlay {
        cleanup_trace_overlays(&applied_overlays)?
    }
    let preview = finish_trace_public_preview(preview_session, run_dir)?;
    let results_path = run_dir.step_file(run_dir::files::TRACE_RESULTS);
    let mut results = if results_path.exists() {
        let mut parsed = parse_trace_results_file(&results_path)?;
        if parsed.rig.is_none() {
            parsed.rig = rig_state;
        }
        parsed.evidence = Some(evidence.clone());
        Some(parsed)
    } else {
        None
    };
    if let Some(parsed) = results.as_mut() {
        parsed.toolchain = Some(toolchain.clone());
        parsed.components = Some(components.clone());
        parsed.dependencies = dependency_provenance;
        parsed.timeline.extend(probe_events);
        parsed.timeline.sort_by_key(|event| event.t_ms);
        apply_trace_preview_metadata(parsed, preview.as_ref());
        append_attach_observations(parsed, run_dir, &attach_observations)?;
        super::spans::apply_span_definitions(parsed, &args.span_definitions);
        super::assertions::apply_temporal_assertions(parsed);
        validate_declared_trace_artifacts(parsed, run_dir, &artifact_dir);
        persist_trace_results(&results_path, parsed)?;
    }

    let status = results
        .as_ref()
        .map(|r| r.status.as_str().to_string())
        .unwrap_or_else(|| {
            if runner_output.success {
                "pass"
            } else {
                "error"
            }
            .to_string()
        });
    let failure = (!runner_output.success && status != "pass")
        .then(|| failure_from_output(&args, &runner_output, Some(&artifact_dir), results.as_ref()));
    let exit_code = if status == "pass" {
        0
    } else if runner_output.success {
        1
    } else {
        runner_output.exit_code
    };
    let rig_id = args.rig_id.as_deref();
    let baseline_root = resolve_trace_baseline_root(&component_path, rig_id)?;
    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;
    let mut hints = non_canonical_evidence_hints(&evidence);
    let has_baseline_items = results
        .as_ref()
        .is_some_and(|parsed| !parsed.span_results.is_empty() || !parsed.assertions.is_empty());

    if args.baseline_flags.baseline && status == "pass" && has_baseline_items {
        if let Some(ref parsed) = results {
            let _ =
                super::baseline::save_baseline(&baseline_root, &args.component_id, parsed, rig_id)?;
        }
    }
    if has_baseline_items && !args.baseline_flags.baseline && !args.baseline_flags.ignore_baseline {
        if let Some(ref parsed) = results {
            if let Some(existing) = super::baseline::load_baseline(&baseline_root, rig_id) {
                let comparison = super::baseline::compare(
                    parsed,
                    &existing,
                    args.regression_threshold_percent,
                    args.regression_min_delta_ms,
                );
                if comparison.regression {
                    baseline_exit_override = Some(1);
                } else if comparison.has_improvements && args.baseline_flags.ratchet {
                    let _ = super::baseline::save_baseline(
                        &baseline_root,
                        &args.component_id,
                        parsed,
                        rig_id,
                    );
                }
                baseline_comparison = Some(comparison);
            }
        }
    }

    let trace_invocation = match rig_id {
        Some(id) => format!(
            "homeboy trace {} {} --rig {}",
            args.component_id, args.scenario_id, id
        ),
        None => format!("homeboy trace {} {}", args.component_id, args.scenario_id),
    };
    if has_baseline_items && !args.baseline_flags.baseline && baseline_comparison.is_none() {
        hints.push(format!(
            "Save trace baseline: {} --baseline",
            trace_invocation
        ));
    }
    if baseline_comparison.is_some() && !args.baseline_flags.ratchet {
        hints.push(format!(
            "Auto-update trace baseline on improvement: {} --ratchet",
            trace_invocation
        ));
    }
    if let Some(ref cmp) = baseline_comparison {
        if cmp.regression {
            hints.push(format!(
                "Trace span regression threshold: {}% and {}ms. Raise them with --regression-threshold=<PCT> or --regression-min-delta-ms=<MS> if expected.",
                cmp.threshold_percent, cmp.min_delta_ms
            ));
        }
    }

    let exit_code = baseline_exit_override.unwrap_or(exit_code);

    Ok(TraceRunWorkflowResult {
        status,
        component: args.component_label,
        exit_code,
        evidence,
        results,
        failure,
        overlays: applied_overlays
            .into_iter()
            .map(|overlay| TraceOverlay {
                variant: overlay.variant,
                component_id: overlay.component_id,
                path: overlay.patch_path.to_string_lossy().to_string(),
                component_path: overlay.component_path.to_string_lossy().to_string(),
                touched_files: overlay.touched_files,
                kept: overlay.keep,
            })
            .collect(),
        baseline_comparison,
        hints: if hints.is_empty() { None } else { Some(hints) },
        toolchain: Some(toolchain),
        components: Some(components),
    })
}

fn non_canonical_evidence_hints(evidence: &TraceEvidenceMetadata) -> Vec<String> {
    if evidence.canonical {
        return Vec::new();
    }
    vec![format!(
        "Non-canonical local evidence mode `{}` is active; do not use this run as reviewer-facing proof until the reported canonicality reasons are fixed.",
        evidence.mode
    )]
}

fn trace_provenance(
    execution_context: Option<&ExtensionExecutionContext>,
    component_path: &str,
) -> (TraceToolchainProvenance, TraceComponentsProvenance) {
    let homeboy = homeboy_git_provenance();
    let target = git_provenance(Path::new(component_path), Some("target"));
    let env_wp_codebox_bin = wp_codebox_bin_env();
    let wp_codebox_path = execution_context
        .filter(|context| {
            context.extension_id.contains("wp-codebox")
                || context
                    .extension_path
                    .to_string_lossy()
                    .contains("wp-codebox")
        })
        .map(|context| context.extension_path.as_path())
        .or_else(|| {
            env_wp_codebox_bin
                .as_ref()
                .map(|(_, value)| Path::new(value.as_str()))
        });
    let mut reasons = Vec::new();
    let mut wp_codebox = wp_codebox_path.map(|path| git_provenance(path, Some("wp_codebox")));

    if let Some((key, _)) = env_wp_codebox_bin.as_ref() {
        if let Some(provenance) = wp_codebox.as_mut() {
            provenance.source = Some(format!("env:{key}"));
        }
    }
    if execution_context.is_none() {
        reasons.push("trace runner used generic local workload discovery".to_string());
    }
    for (label, provenance) in [("homeboy", &homeboy), ("target", &target)] {
        push_git_provenance_reasons(label, provenance, &mut reasons);
    }
    if let Some(provenance) = wp_codebox.as_ref() {
        push_git_provenance_reasons("WP Codebox", provenance, &mut reasons);
    }
    if wp_codebox.is_none() {
        reasons.push("WP Codebox checkout was not resolved for this trace run".to_string());
    }
    let canonical = reasons.is_empty();
    let mut runtime_assets = BTreeMap::new();
    runtime_assets.insert(
        "browser_runtime".to_string(),
        browser_runtime_asset_provenance(),
    );

    (
        TraceToolchainProvenance {
            canonical,
            mode: if canonical {
                "canonical"
            } else {
                "development"
            }
            .to_string(),
            reasons,
            homeboy,
            wp_codebox,
            node: command_version("node", &["--version"]),
            runtime_assets,
        },
        TraceComponentsProvenance {
            target,
            dependencies: Vec::new(),
        },
    )
}

fn push_git_provenance_reasons(
    label: &str,
    provenance: &TraceGitProvenance,
    reasons: &mut Vec<String>,
) {
    if provenance.sha.is_none() || provenance.dirty.is_none() {
        reasons.push(format!(
            "{label} checkout git provenance is incomplete for {}",
            provenance.path
        ));
    } else if provenance.dirty == Some(true) {
        reasons.push(format!(
            "{label} checkout is dirty for trace toolchain provenance: {}",
            provenance.path
        ));
    }
}

fn wp_codebox_bin_env() -> Option<(String, String)> {
    ["HOMEBOY_WP_CODEBOX_BIN", "HOMEBOY_SETTINGS_WP_CODEBOX_BIN"]
        .into_iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| (key.to_string(), value))
        })
}

fn mark_non_canonical(toolchain: &mut TraceToolchainProvenance, reason: &str) {
    toolchain.canonical = false;
    toolchain.mode = "development".to_string();
    if !toolchain.reasons.iter().any(|item| item == reason) {
        toolchain.reasons.push(reason.to_string());
    }
}

fn git_provenance(path: &Path, source: Option<&str>) -> TraceGitProvenance {
    let probe_path = git_probe_path(path);
    let git_root = crate::core::git::get_git_root(&probe_path.to_string_lossy())
        .ok()
        .map(PathBuf::from)
        .unwrap_or(probe_path);
    TraceGitProvenance {
        path: git_root.to_string_lossy().to_string(),
        sha: git_stdout(&git_root, &["rev-parse", "HEAD"]),
        branch: crate::core::git::current_branch(&git_root),
        dirty: git_dirty_state(&git_root),
        source: source.map(ToString::to_string),
    }
}

fn git_probe_path(path: &Path) -> PathBuf {
    if path.is_file() {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn homeboy_git_provenance() -> TraceGitProvenance {
    let exe_path = std::env::current_exe().ok();
    let exe_parent = exe_path.as_deref().and_then(Path::parent);
    let manifest_dir = env!(concat!("CARGO", "_MANIFEST_DIR"));
    let provenance = exe_parent
        .map(|path| git_provenance(path, Some("homeboy")))
        .filter(|provenance| provenance.sha.is_some());

    provenance.unwrap_or_else(|| git_provenance(Path::new(manifest_dir), Some("homeboy")))
}

fn git_stdout(path: &Path, args: &[&str]) -> Option<String> {
    crate::core::git::run_git(path, args, "trace provenance git probe")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn git_dirty_state(path: &Path) -> Option<bool> {
    crate::core::git::run_git(
        path,
        &["status", "--porcelain=v1"],
        "trace provenance git status",
    )
    .ok()
    .map(|status| !status.trim().is_empty())
}

fn command_version(command: &str, args: &[&str]) -> Option<String> {
    Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn browser_runtime_asset_provenance() -> TraceRuntimeAssetProvenance {
    TraceRuntimeAssetProvenance {
        present: false,
        mode: Some("jspi".to_string()),
        version: None,
        path: None,
    }
}

fn validate_declared_trace_artifacts(
    results: &mut TraceResults,
    run_dir: &RunDir,
    artifact_dir: &Path,
) {
    let missing = results
        .artifacts
        .iter()
        .filter(|artifact| {
            resolve_declared_trace_artifact_path(&artifact.path, run_dir, artifact_dir).is_none()
        })
        .map(|artifact| artifact.path.clone())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return;
    }

    results.status = TraceStatus::Error;
    results.failure = Some(format!(
        "missing declared trace artifact{}: {}",
        if missing.len() == 1 { "" } else { "s" },
        missing.join(", ")
    ));
    for path in missing {
        results.assertions.push(TraceAssertion {
            id: format!("trace_artifact_exists:{}", path),
            status: TraceAssertionStatus::Error,
            message: Some(format!("Declared trace artifact is missing: {path}")),
            details: Some(serde_json::json!({ "path": path })),
        });
    }
}

pub fn resolve_declared_trace_artifact_path(
    path: &str,
    run_dir: &RunDir,
    artifact_dir: &Path,
) -> Option<PathBuf> {
    let relative = Path::new(path);
    if relative.is_absolute() {
        return relative.exists().then(|| relative.to_path_buf());
    }
    if relative
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }

    [run_dir.path().join(relative), artifact_dir.join(relative)]
        .into_iter()
        .find(|candidate| candidate.exists())
}

fn persist_trace_results(path: &Path, results: &TraceResults) -> Result<()> {
    let content = serde_json::to_string_pretty(results).map_err(|e| {
        Error::internal_json(
            format!("Failed to serialize trace results JSON: {}", e),
            Some("trace.results.serialize".to_string()),
        )
    })?;
    std::fs::write(path, content).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write trace results file {}: {}",
                path.display(),
                e
            ),
            Some("trace.results.write".to_string()),
        )
    })
}

pub fn run_trace_list_workflow(
    component: &Component,
    args: TraceListWorkflowArgs,
    run_dir: &RunDir,
) -> Result<TraceList> {
    if component.has_script(ExtensionCapability::Trace) {
        let source_path = crate::core::extension::component_script::source_path(
            component,
            args.path_override.as_deref(),
        );
        let output = crate::core::extension::component_script::run_component_scripts_with_run_dir(
            component,
            ExtensionCapability::Trace,
            &source_path,
            run_dir,
            true,
            &[("HOMEBOY_TRACE_LIST_ONLY".to_string(), "1".to_string())],
            &[],
        )?;
        return trace_list_from_output(run_dir, TraceListOutput::from(output));
    }

    let execution_context = match resolve_execution_context(component, ExtensionCapability::Trace) {
        Ok(execution_context) => Some(execution_context),
        Err(error) if trace_is_unclaimed(&error) => None,
        Err(error) => return Err(error),
    };
    let runner_args = TraceRunWorkflowArgs {
        component_label: args.component_label.clone(),
        component_id: args.component_id,
        path_override: args.path_override,
        settings: args.settings,
        runner_inputs: args.runner_inputs,
        scenario_id: String::new(),
        json_summary: false,
        rig_id: args.rig_id,
        overlays: Vec::new(),
        keep_overlay: false,
        span_definitions: Vec::new(),
        baseline_flags: BaselineFlags {
            baseline: false,
            ignore_baseline: true,
            ratchet: false,
        },
        regression_threshold_percent: super::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
        regression_min_delta_ms: super::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
        canonical_policy: TraceCanonicalPolicy::Development,
        checkout_provenance: None,
    };
    let output = build_trace_runner(
        execution_context.as_ref(),
        component,
        &runner_args,
        run_dir,
        true,
    )?;
    trace_list_from_output(run_dir, TraceListOutput::from(output))
}

struct TraceListOutput {
    exit_code: i32,
    success: bool,
    stdout: String,
    stderr: String,
}

impl From<crate::core::extension::component_script::ComponentScriptOutput> for TraceListOutput {
    fn from(output: crate::core::extension::component_script::ComponentScriptOutput) -> Self {
        Self {
            exit_code: output.exit_code,
            success: output.success,
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

impl From<RunnerOutput> for TraceListOutput {
    fn from(output: RunnerOutput) -> Self {
        Self {
            exit_code: output.exit_code,
            success: output.success,
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

fn trace_list_from_output(run_dir: &RunDir, output: TraceListOutput) -> Result<TraceList> {
    if output.success {
        return parse_trace_list_output(run_dir, &output.stdout);
    }

    Err(trace_list_error(
        output.exit_code,
        &output.stdout,
        &output.stderr,
    ))
}

fn trace_list_error(exit_code: i32, stdout: &str, stderr: &str) -> Error {
    Error::validation_invalid_argument(
        "trace_list",
        format!("trace scenario discovery failed with exit code {exit_code}"),
        Some(format!("stdout:\n{stdout}\n\nstderr:\n{stderr}")),
        None,
    )
}

fn parse_trace_list_output(run_dir: &RunDir, stdout: &str) -> Result<TraceList> {
    let results_path = run_dir.step_file(run_dir::files::TRACE_RESULTS);
    if results_path.exists() {
        let content = std::fs::read_to_string(&results_path).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to read trace list file {}: {}",
                    results_path.display(),
                    e
                ),
                Some("trace.list.read".to_string()),
            )
        })?;
        return parse_trace_list_str(&content);
    }

    parse_trace_list_str(stdout)
}

pub(crate) fn build_trace_runner(
    execution_context: Option<&ExtensionExecutionContext>,
    component: &Component,
    args: &TraceRunWorkflowArgs,
    run_dir: &RunDir,
    list_only: bool,
) -> Result<RunnerOutput> {
    let artifact_dir = run_dir.path().join("artifacts");
    std::fs::create_dir_all(&artifact_dir).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to create trace artifact dir {}: {}",
                artifact_dir.display(),
                e
            ),
            Some("trace.artifacts.create".to_string()),
        )
    })?;

    let Some(execution_context) = execution_context else {
        preflight_trace_runner_capabilities(None, &args.runner_inputs.runner_capabilities)?;
        return run_generic_trace_runner(component, args, run_dir, &artifact_dir, list_only);
    };

    preflight_trace_runner_capabilities(
        Some(execution_context),
        &args.runner_inputs.runner_capabilities,
    )?;

    let mut runner = build_scenario_runner(ScenarioRunnerOptions {
        execution_context,
        component,
        path_override: args.path_override.clone(),
        settings: &args.settings,
        settings_json: &args.runner_inputs.json_settings,
        run_dir,
        results_env: Some((
            "HOMEBOY_TRACE_RESULTS_FILE",
            run_dir.step_file(run_dir::files::TRACE_RESULTS),
        )),
        scenario_env: Some(("HOMEBOY_TRACE_SCENARIO", &args.scenario_id)),
        artifact_env: Some(("HOMEBOY_TRACE_ARTIFACT_DIR", &artifact_dir)),
        list_only_env: Some(("HOMEBOY_TRACE_LIST_ONLY", list_only)),
        extra_workloads_env: Some((
            "HOMEBOY_TRACE_EXTRA_WORKLOADS",
            &args.runner_inputs.workload_paths,
            "trace_workloads",
        )),
        invocation_requirements: args.runner_inputs.invocation_requirements.clone(),
    })?;

    if let Some(rig_id) = &args.rig_id {
        runner = runner.env("HOMEBOY_TRACE_RIG_ID", rig_id);
    }
    if let Some(path) = &args.path_override {
        runner = runner.env("HOMEBOY_TRACE_COMPONENT_PATH", path);
    }
    if !args.runner_inputs.attachments.is_empty() {
        let attachments_json =
            serde_json::to_string(&args.runner_inputs.attachments).map_err(|e| {
                Error::internal_json(
                    format!("Failed to serialize trace attachments: {e}"),
                    Some("trace.attach.serialize".to_string()),
                )
            })?;
        runner = runner.env("HOMEBOY_TRACE_ATTACHMENTS", &attachments_json);
    }
    for (key, value) in &args.runner_inputs.env {
        runner = runner.env(key, value);
    }

    runner.run()
}

pub fn trace_is_unclaimed(error: &Error) -> bool {
    error.code == ErrorCode::ExtensionUnsupported
        || (error.code == ErrorCode::ValidationInvalidArgument
            && error
                .message
                .contains("has no linked extensions that provide trace support"))
}

fn trace_probes_with_fswatch_attachments(
    probes: &[TraceProbeConfig],
    attachments: &[TraceAttachment],
) -> Vec<TraceProbeConfig> {
    let mut merged = probes.to_vec();
    for attachment in attachments {
        if attachment.kind != "fswatch" {
            continue;
        }
        let already_watched = merged.iter().any(|probe| match probe {
            TraceProbeConfig::FileWatch { path, .. } => path == &attachment.target,
            _ => false,
        });
        if !already_watched {
            merged.push(TraceProbeConfig::FileWatch {
                path: attachment.target.clone(),
                interval_ms: None,
            });
        }
    }
    merged
}

/// Resolve the directory that holds the trace baseline `homeboy.json`.
///
/// Non-rig traces keep the historical component-local behavior — the baseline
/// is co-located with the project's `homeboy.json` in the component checkout.
/// Rig-owned traces store baselines in the rig state directory so that
/// `homeboy trace --rig <id>` against an unrelated component checkout (e.g.
/// `Automattic/studio`) never creates or mutates a `homeboy.json` inside that
/// repo. See Extra-Chill/homeboy#2329.
fn resolve_trace_baseline_root(component_path: &str, rig_id: Option<&str>) -> Result<PathBuf> {
    match rig_id {
        Some(id) => {
            let root = paths::rig_baseline_root(id)?;
            std::fs::create_dir_all(&root).map_err(|e| {
                Error::internal_io(
                    format!(
                        "Failed to create rig baseline root {}: {}",
                        root.display(),
                        e
                    ),
                    Some("trace.baseline.rig_root.create".to_string()),
                )
            })?;
            Ok(root)
        }
        None => Ok(PathBuf::from(component_path)),
    }
}

fn failure_from_output(
    args: &TraceRunWorkflowArgs,
    output: &RunnerOutput,
    artifact_dir: Option<&Path>,
    results: Option<&TraceResults>,
) -> TraceRunFailure {
    let child = output.child_resource.as_ref().map(|summary| &summary.child);
    let last_event = results.and_then(last_observed_homeboy_event);
    TraceRunFailure {
        component_id: args.component_id.clone(),
        path_override: args.path_override.clone(),
        scenario_id: args.scenario_id.clone(),
        exit_code: output.exit_code,
        stderr_excerpt: stderr_tail(&output.stderr),
        current_phase: last_event.clone(),
        child_pid: child.map(|child| child.root_pid),
        child_command: child.map(|child| child.command_label.clone()),
        recipe_path: recipe_path_from_args(args),
        artifact_root: artifact_dir.map(|path| path.to_string_lossy().to_string()),
        last_observed_homeboy_event: last_event,
        cleanup_succeeded: output.child_resource.as_ref().map(|_| true),
    }
}

fn recipe_path_from_args(args: &TraceRunWorkflowArgs) -> Option<String> {
    args.runner_inputs
        .json_settings
        .iter()
        .find_map(|(key, value)| {
            key.to_ascii_lowercase()
                .contains("recipe")
                .then(|| value.as_str().map(ToString::to_string))
                .flatten()
        })
        .or_else(|| {
            args.runner_inputs
                .workload_paths
                .first()
                .map(|path| path.to_string_lossy().to_string())
        })
}

fn last_observed_homeboy_event(results: &TraceResults) -> Option<String> {
    results
        .timeline
        .iter()
        .max_by_key(|event| event.t_ms)
        .map(|event| format!("{}.{}", event.source, event.event))
}

fn start_trace_public_preview(
    args: &mut TraceRunWorkflowArgs,
) -> Result<Option<TracePublicPreviewSession>> {
    let Some(spec) = args.runner_inputs.public_preview.clone() else {
        return Ok(None);
    };
    let session = TracePublicPreviewSession::start(&spec)?;
    args.runner_inputs.env.extend(session.env_vars()?);
    Ok(Some(session))
}

fn finish_trace_public_preview(
    session: Option<TracePublicPreviewSession>,
    run_dir: &RunDir,
) -> Result<Option<TracePreviewMetadata>> {
    let Some(session) = session else {
        return Ok(None);
    };
    let metadata = session.finish();
    let artifact_dir = run_dir.path().join("artifacts");
    std::fs::create_dir_all(&artifact_dir).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to create trace preview artifact dir {}: {}",
                artifact_dir.display(),
                e
            ),
            Some("trace.preview.artifact_dir".to_string()),
        )
    })?;
    let path = artifact_dir.join("preview.json");
    let content = serde_json::to_string_pretty(&metadata).map_err(|e| {
        Error::internal_json(
            format!("Failed to serialize trace preview artifact: {e}"),
            Some("trace.preview.artifact_serialize".to_string()),
        )
    })?;
    std::fs::write(&path, content).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write trace preview artifact {}: {e}",
                path.display()
            ),
            Some("trace.preview.artifact_write".to_string()),
        )
    })?;
    Ok(Some(metadata))
}

fn apply_trace_preview_metadata(
    results: &mut TraceResults,
    preview: Option<&TracePreviewMetadata>,
) {
    let Some(preview) = preview else {
        return;
    };
    results.preview = Some(preview.clone());
    if !results
        .artifacts
        .iter()
        .any(|artifact| artifact.path == "artifacts/preview.json")
    {
        results.artifacts.push(super::parsing::TraceArtifact {
            label: "Public preview metadata".to_string(),
            path: "artifacts/preview.json".to_string(),
            kind: None,
        });
    }
    if preview.require_https {
        let status = if preview.window_is_secure_context {
            TraceAssertionStatus::Pass
        } else {
            results.status = TraceStatus::Error;
            TraceAssertionStatus::Error
        };
        results.assertions.push(TraceAssertion {
            id: "public_preview.secure_context".to_string(),
            status,
            message: Some(format!(
                "Browser effective origin `{}` secure_context={}",
                preview.browser_effective_origin, preview.window_is_secure_context
            )),
            details: Some(serde_json::json!({
                "requested_mode": preview.requested_mode,
                "local_origin": preview.local_origin,
                "public_origin": preview.public_origin,
                "browser_effective_origin": preview.browser_effective_origin,
                "window_location_origin": preview.window_location_origin,
                "window_is_secure_context": preview.window_is_secure_context
            })),
        });
    }
}

#[cfg(test)]
mod run_tests {
    include!("run_tests.inc");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_trace_runner() {
        let temp = tempfile::tempdir().unwrap();
        let component = test_component(temp.path());
        let run_dir = RunDir::create().unwrap();
        let output = build_trace_runner(
            None,
            &component,
            &test_run_args(temp.path()),
            &run_dir,
            false,
        )
        .unwrap();
        assert!(!output.success);
        assert_eq!(output.exit_code, 3);
        run_dir.cleanup();
    }

    #[test]
    fn test_run_trace_list_workflow() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            let component = test_component(temp.path());
            let run_dir = RunDir::create().unwrap();
            let list = run_trace_list_workflow(
                &component,
                TraceListWorkflowArgs {
                    component_label: "example".to_string(),
                    component_id: "example".to_string(),
                    path_override: Some(temp.path().to_string_lossy().to_string()),
                    settings: Vec::new(),
                    runner_inputs: TraceRunnerInputs::default(),
                    rig_id: None,
                },
                &run_dir,
            )
            .unwrap();
            assert!(list.scenarios.is_empty());
            run_dir.cleanup();
        });
    }

    #[test]
    fn test_run_trace_workflow() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            let component = test_component(temp.path());
            let run_dir = RunDir::create().unwrap();
            let result =
                run_trace_workflow(&component, test_run_args(temp.path()), &run_dir, None).unwrap();
            assert_eq!(result.status, "error");
            assert_eq!(result.exit_code, 3);
            run_dir.cleanup();
        });
    }

    #[test]
    fn trace_failure_records_child_cleanup_context() {
        let temp = tempfile::tempdir().unwrap();
        let artifact_dir = temp.path().join("artifacts");
        let mut args = test_run_args(temp.path());
        args.runner_inputs.json_settings.push((
            "recipe_path".to_string(),
            serde_json::Value::String("recipes/stripe-ece.yml".to_string()),
        ));
        let output = RunnerOutput {
            exit_code: 143,
            success: false,
            stdout: String::new(),
            stderr: "Homeboy interrupted by signal 15".to_string(),
            child_resource: Some(
                crate::core::engine::resource::ExtensionChildResourceSummary {
                    child: crate::core::engine::resource::ChildProcessIdentity {
                        root_pid: 4242,
                        command_label: "wp-codebox recipe-run recipes/stripe-ece.yml".to_string(),
                    },
                    phase: None,
                    started_at: "2026-06-06T00:00:00Z".to_string(),
                    finished_at: "2026-06-06T00:00:01Z".to_string(),
                    duration_ms: 1000,
                    sampled_peak_rss_bytes: None,
                    sampled_peak_cpu_percent: None,
                    sampled_peak_at_ms: None,
                    sampled_peak_child_count: None,
                    samples: Vec::new(),
                    warnings: Vec::new(),
                },
            ),
        };
        let results = TraceResults {
            component_id: "example".to_string(),
            scenario_id: "fixture".to_string(),
            status: TraceStatus::Error,
            summary: None,
            failure: None,
            rig: None,
            evidence: None,
            preview: None,
            timeline: vec![crate::core::observation::timeline::ObservationEvent {
                t_ms: 250,
                source: "homeboy".to_string(),
                event: "recipe.waiting".to_string(),
                data: BTreeMap::new(),
            }],
            span_definitions: Vec::new(),
            span_results: Vec::new(),
            metrics: Default::default(),
            assertions: Vec::new(),
            temporal_assertions: Vec::new(),
            artifacts: Vec::new(),
            dependencies: Vec::new(),
            toolchain: None,
            components: None,
        };

        let failure = failure_from_output(&args, &output, Some(&artifact_dir), Some(&results));

        assert_eq!(failure.child_pid, Some(4242));
        assert_eq!(
            failure.child_command.as_deref(),
            Some("wp-codebox recipe-run recipes/stripe-ece.yml")
        );
        assert_eq!(
            failure.recipe_path.as_deref(),
            Some("recipes/stripe-ece.yml")
        );
        assert_eq!(
            failure.artifact_root.as_deref(),
            Some(artifact_dir.to_string_lossy().as_ref())
        );
        assert_eq!(
            failure.last_observed_homeboy_event.as_deref(),
            Some("homeboy.recipe.waiting")
        );
        assert_eq!(
            failure.current_phase.as_deref(),
            Some("homeboy.recipe.waiting")
        );
        assert_eq!(failure.cleanup_succeeded, Some(true));
    }

    #[test]
    fn test_trace_is_unclaimed() {
        let unsupported = Error::new(
            ErrorCode::ExtensionUnsupported,
            "No extension provider configured for component 'example'",
            serde_json::json!({}),
        );
        assert!(trace_is_unclaimed(&unsupported));
    }

    #[test]
    fn resolve_trace_baseline_root_without_rig_returns_component_path() {
        let temp = tempfile::tempdir().unwrap();
        let component_path = temp.path().to_string_lossy().to_string();
        let root = resolve_trace_baseline_root(&component_path, None).unwrap();
        assert_eq!(root, PathBuf::from(&component_path));
        // Crucially, no homeboy.json gets created in the component checkout
        // just by resolving — that only happens when a baseline is saved.
        assert!(!temp.path().join("homeboy.json").exists());
    }

    #[test]
    fn resolve_trace_baseline_root_with_rig_uses_rig_state_dir_and_skips_component_path() {
        let temp = tempfile::tempdir().unwrap();
        let component_path = temp.path().to_string_lossy().to_string();
        let rig_id = format!("__hb-trace-baseline-test-{}", std::process::id());

        let root = resolve_trace_baseline_root(&component_path, Some(&rig_id)).unwrap();

        assert!(
            root.ends_with(format!("{}.state/baselines", rig_id)),
            "rig baseline root should live under <id>.state/baselines, got {}",
            root.display()
        );
        assert!(
            root.exists(),
            "rig baseline root should be created on resolve"
        );
        assert!(
            !root.starts_with(temp.path()),
            "rig baseline root must not live inside the component checkout"
        );
        assert!(
            !temp.path().join("homeboy.json").exists(),
            "resolving a rig baseline root must not touch component homeboy.json"
        );

        // Cleanup: best-effort remove the rig state dir we created.
        if let Some(state_dir) = root.parent() {
            let _ = std::fs::remove_dir_all(state_dir);
        }
    }

    #[test]
    fn rig_save_baseline_does_not_write_component_homeboy_json() {
        use crate::core::extension::trace::baseline;
        use crate::core::extension::trace::parsing::{
            TraceResults, TraceSpanResult, TraceSpanStatus, TraceStatus,
        };

        let temp = tempfile::tempdir().unwrap();
        let component_path = temp.path().to_string_lossy().to_string();
        let rig_id = format!("__hb-trace-save-test-{}", std::process::id());

        let baseline_root = resolve_trace_baseline_root(&component_path, Some(&rig_id)).unwrap();

        let results = TraceResults {
            component_id: "studio".to_string(),
            scenario_id: "create-site".to_string(),
            status: TraceStatus::Pass,
            summary: None,
            failure: None,
            rig: None,
            evidence: None,
            timeline: Vec::new(),
            span_definitions: Vec::new(),
            span_results: vec![TraceSpanResult {
                id: "submit_to_cli".to_string(),
                from: "ui.submit".to_string(),
                to: "cli.start".to_string(),
                status: TraceSpanStatus::Ok,
                duration_ms: Some(120),
                from_t_ms: Some(0),
                to_t_ms: Some(120),
                missing: Vec::new(),
                message: None,
            }],
            assertions: Vec::new(),
            metrics: Default::default(),
            temporal_assertions: Vec::new(),
            artifacts: Vec::new(),
            toolchain: None,
            components: None,
            dependencies: Vec::new(),
            preview: None,
        };

        let written = baseline::save_baseline(&baseline_root, "studio", &results, Some(&rig_id))
            .expect("rig baseline saves into rig state dir");

        assert!(
            written.starts_with(&baseline_root),
            "rig baseline must be written under the rig baseline root, got {}",
            written.display()
        );
        assert!(
            !temp.path().join("homeboy.json").exists(),
            "rig baseline save must not write homeboy.json into the component checkout"
        );

        let loaded = baseline::load_baseline(&baseline_root, Some(&rig_id))
            .expect("rig baseline loads from rig state dir");
        assert_eq!(loaded.metadata.spans[0].id, "submit_to_cli");

        if let Some(state_dir) = baseline_root.parent() {
            let _ = std::fs::remove_dir_all(state_dir);
        }
    }

    #[test]
    fn fswatch_attachments_add_file_watch_probes_without_duplicates() {
        let attachment = TraceAttachment::parse("fswatch:/tmp/auth.json").unwrap();
        let existing_probe = TraceProbeConfig::FileWatch {
            path: "/tmp/auth.json".to_string(),
            interval_ms: Some(50),
        };

        let merged =
            trace_probes_with_fswatch_attachments(&[existing_probe.clone()], &[attachment.clone()]);
        assert_eq!(merged, vec![existing_probe]);

        let merged = trace_probes_with_fswatch_attachments(&[], &[attachment]);
        assert_eq!(
            merged,
            vec![TraceProbeConfig::FileWatch {
                path: "/tmp/auth.json".to_string(),
                interval_ms: None,
            }]
        );
    }

    #[test]
    fn git_dirty_state_reports_clean_checkout_as_known_clean() {
        let temp = tempfile::tempdir().unwrap();
        let init = Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .expect("git init runs");
        assert!(init.status.success());

        assert_eq!(git_dirty_state(temp.path()), Some(false));

        std::fs::write(temp.path().join("untracked.txt"), "changed").unwrap();

        assert_eq!(git_dirty_state(temp.path()), Some(true));
    }

    #[test]
    fn git_provenance_resolves_file_paths_to_checkout_root() {
        let temp = tempfile::tempdir().unwrap();
        let init = Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .expect("git init runs");
        assert!(init.status.success());
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp.path())
            .output()
            .expect("git config user.email runs");
        Command::new("git")
            .args(["config", "user.name", "Homeboy Test"])
            .current_dir(temp.path())
            .output()
            .expect("git config user.name runs");
        let bin = temp.path().join("packages/cli/dist/index.js");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/usr/bin/env node\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(temp.path())
            .output()
            .expect("git add runs");
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(temp.path())
            .output()
            .expect("git commit runs");

        let provenance = git_provenance(&bin, Some("wp_codebox"));

        assert_eq!(
            provenance.path,
            temp.path().canonicalize().unwrap().to_string_lossy()
        );
        assert!(provenance.sha.is_some());
        assert_eq!(provenance.dirty, Some(false));
    }

    #[test]
    fn dirty_git_provenance_makes_toolchain_non_canonical() {
        let mut reasons = Vec::new();
        let provenance = TraceGitProvenance {
            path: "/tmp/homeboy".to_string(),
            sha: Some("abc123".to_string()),
            branch: Some("main".to_string()),
            dirty: Some(true),
            source: Some("homeboy".to_string()),
        };

        push_git_provenance_reasons("homeboy", &provenance, &mut reasons);

        assert_eq!(
            reasons,
            vec!["homeboy checkout is dirty for trace toolchain provenance: /tmp/homeboy"]
        );
    }

    fn test_component(path: &std::path::Path) -> Component {
        Component {
            id: "example".to_string(),
            local_path: path.to_string_lossy().to_string(),
            ..Default::default()
        }
    }

    fn test_run_args(path: &std::path::Path) -> TraceRunWorkflowArgs {
        TraceRunWorkflowArgs {
            component_label: "example".to_string(),
            component_id: "example".to_string(),
            path_override: Some(path.to_string_lossy().to_string()),
            settings: Vec::new(),
            runner_inputs: TraceRunnerInputs::default(),
            scenario_id: "missing".to_string(),
            json_summary: false,
            rig_id: None,
            overlays: Vec::new(),
            keep_overlay: false,
            span_definitions: Vec::new(),
            baseline_flags: BaselineFlags {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            regression_threshold_percent:
                super::super::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
            regression_min_delta_ms: super::super::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
            canonical_policy: TraceCanonicalPolicy::Development,
            checkout_provenance: None,
        }
    }
}
