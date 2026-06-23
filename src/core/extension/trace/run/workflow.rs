//! Top-level trace run orchestration (component-script and runner paths).

use std::path::Path;
use std::time::Instant;

use crate::core::component::Component;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, Result};
use crate::core::extension::{
    resolve_execution_context, stderr_tail, ExtensionCapability, ExtensionExecutionContext,
};
use crate::core::rig::RigStateSnapshot;

use super::super::attach::{append_attach_observations, observe_trace_attachments};
use super::super::canonicality::{
    evaluate_trace_canonicality, refused_trace_result, TraceCanonicalPolicy,
};
use super::super::overlay::{
    acquire_trace_overlay_locks, apply_trace_overlays, cleanup_after_overlay_error,
    cleanup_trace_overlays,
};
use super::super::parsing::parse_trace_results_file;
use super::super::preflight::{preflight_trace_dependencies, preflight_trace_runner_capabilities};
use super::super::probes::ActiveTraceProbes;
use super::artifacts::{persist_trace_results, validate_declared_trace_artifacts};
use super::preview::{
    apply_trace_preview_metadata, finish_trace_public_preview, start_trace_public_preview,
};
use super::provenance::{mark_non_canonical, non_canonical_evidence_hints, trace_provenance};
use super::runner::{
    build_trace_runner, failure_from_output, resolve_trace_baseline_root, trace_is_unclaimed,
    trace_probes_with_fswatch_attachments,
};
use super::types::{TraceOverlay, TraceRunFailure, TraceRunWorkflowArgs, TraceRunWorkflowResult};

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
    let (mut toolchain, components) = trace_provenance(None, &component_path, &args)?;
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
    let preview_session = start_trace_public_preview(&mut args, run_dir)?;
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
        super::super::spans::apply_span_definitions(parsed, &args.span_definitions);
        super::super::assertions::apply_temporal_assertions(parsed);
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
        recipe_path: super::runner::recipe_path_from_args(&args),
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

pub(super) fn run_trace_workflow_with_context(
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
    let (toolchain, components) = trace_provenance(execution_context, &component_path, &args)?;
    let _overlay_locks = if args.overlays.is_empty() {
        None
    } else {
        Some(acquire_trace_overlay_locks(&args.overlays, run_dir)?)
    };
    let applied_overlays = apply_trace_overlays(&args.overlays, args.keep_overlay)?;
    let preview_session = start_trace_public_preview(&mut args, run_dir)?;
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
        super::super::spans::apply_span_definitions(parsed, &args.span_definitions);
        super::super::assertions::apply_temporal_assertions(parsed);
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
            let _ = super::super::baseline::save_baseline(
                &baseline_root,
                &args.component_id,
                parsed,
                rig_id,
            )?;
        }
    }
    if has_baseline_items && !args.baseline_flags.baseline && !args.baseline_flags.ignore_baseline {
        if let Some(ref parsed) = results {
            if let Some(existing) = super::super::baseline::load_baseline(&baseline_root, rig_id) {
                let comparison = super::super::baseline::compare(
                    parsed,
                    &existing,
                    args.regression_threshold_percent,
                    args.regression_min_delta_ms,
                );
                if comparison.regression {
                    baseline_exit_override = Some(1);
                } else if comparison.has_improvements && args.baseline_flags.ratchet {
                    let _ = super::super::baseline::save_baseline(
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
