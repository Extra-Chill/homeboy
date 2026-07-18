//! Compatibility adapter for the legacy passive-observation CLI.
//!
//! Capture itself belongs to the typed Trace contract so rig-owned and ad-hoc
//! evidence use one probe implementation.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use serde::Serialize;

use crate::command_contract::{CommandJsonFamily, CommandOutputFileMode};
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::engine::run_dir::{self, RunDir};
use homeboy::core::git::short_head_revision_at;
use homeboy::core::io::{write_output_file_atomically, OutputWriteOptions};
use homeboy::core::observation::{ActiveObservation, NewRunRecord, RunStatus};
use homeboy::core::Error;
use homeboy_extension::trace::{
    PassiveTraceCapture, TraceArtifact, TraceProbeConfig, TraceResults,
};

use super::utils::args::PositionalComponentArgs;
use super::utils::response::actionable_metadata_value_for_run_ref;
use super::{adapter, CmdResult, GlobalArgs};

const DEFAULT_DURATION: &str = "30s";
const DEFAULT_PROCESS_WATCH_INTERVAL: &str = "1s";

#[derive(Args, Clone)]
pub struct ObserveArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,
    #[arg(long, default_value = DEFAULT_DURATION, value_parser = parse_duration)]
    pub duration: Duration,
    #[arg(long = "tail-log", value_name = "PATH")]
    pub tail_logs: Vec<PathBuf>,
    #[arg(long, value_name = "REGEX")]
    pub grep: Option<String>,
    #[arg(long = "watch-process", value_name = "REGEX")]
    pub watch_processes: Vec<String>,
    #[arg(long = "watch-process-interval", default_value = DEFAULT_PROCESS_WATCH_INTERVAL, value_parser = parse_duration)]
    pub watch_process_interval: Duration,
    #[arg(long = "probe", value_name = "JSON")]
    pub probes: Vec<String>,
}

#[derive(Serialize)]
pub struct ObserveOutput {
    pub command: &'static str,
    pub run_id: String,
    pub component_id: String,
    pub status: String,
    pub duration_ms: u64,
    pub event_count: usize,
    pub artifact_path: String,
    pub hints: Vec<String>,
    #[serde(
        rename = "_homeboy_actionable",
        skip_serializing_if = "Option::is_none"
    )]
    pub actionable: Option<serde_json::Value>,
}

pub(crate) fn adapter(
    output_file_mode: CommandOutputFileMode,
) -> adapter::TypedCommandAdapter<ObserveArgs> {
    adapter::TypedCommandAdapter::json_only(CommandJsonFamily::Quality, output_file_mode, run_json)
}

fn run_json(args: ObserveArgs, global: &GlobalArgs) -> adapter::JsonHandlerResult {
    crate::commands::utils::response::map_cmd_result_to_json(run(args, global))
}

pub fn run(args: ObserveArgs, _global: &GlobalArgs) -> CmdResult<ObserveOutput> {
    let probes = trace_probes(&args)?;
    let ctx = execution_context::resolve(&ResolveOptions {
        component_id: args.comp.component.clone(),
        path_override: args.comp.path.clone(),
        ..Default::default()
    })?;
    let run_dir = RunDir::create()?;
    let trace_path = run_dir.step_file(run_dir::files::TRACE_RESULTS);
    let observation = ActiveObservation::start(
        NewRunRecord::builder("observe")
            .component_id(ctx.component_id.clone())
            .command(observe_command(&args))
            .cwd_path(&ctx.source_path)
            .current_homeboy_version()
            .git_sha(short_head_revision_at(&ctx.source_path))
            .metadata(serde_json::json!({ "duration_ms": duration_millis(args.duration), "probes": probes }))
            .build(),
    )?;

    let capture = PassiveTraceCapture {
        duration: args.duration,
        probes,
    };
    let (mut results, failure) =
        match capture.capture(ctx.component_id.clone(), "observe".to_string()) {
            Ok(results) => (results, None),
            Err(error) => (
                error_results(ctx.component_id.clone(), error.to_string()),
                Some(error.to_string()),
            ),
        };
    results.artifacts.push(TraceArtifact {
        label: "observe timeline".to_string(),
        path: trace_path.to_string_lossy().to_string(),
        kind: None,
    });
    write_trace_results(&trace_path, &results)?;
    let artifact =
        observation
            .store()
            .record_artifact(observation.run_id(), "trace-results", &trace_path)?;
    let status = if failure.is_some() {
        RunStatus::Error
    } else {
        RunStatus::Pass
    };
    let finished = observation.store().finish_run(
        observation.run_id(),
        status,
        Some(serde_json::json!({
            "duration_ms": duration_millis(args.duration),
            "event_count": results.timeline.len(),
            "trace_results_artifact_id": artifact.id,
            "trace_results_path": artifact.path,
            "failure": failure,
        })),
    )?;
    run_dir.cleanup();

    Ok((
        ObserveOutput {
            command: "observe",
            run_id: observation.run_id().to_string(),
            component_id: ctx.component_id,
            status: finished.status,
            duration_ms: duration_millis(args.duration),
            event_count: results.timeline.len(),
            artifact_path: artifact.path,
            hints: vec![
                format!("View this run: homeboy runs show {}", finished.id),
                "List observe runs: homeboy runs list --kind observe".to_string(),
            ],
            actionable: Some(actionable_metadata_value_for_run_ref(
                observation.run_id().to_string(),
                "observe",
                "homeboy-observe",
            )),
        },
        if status == RunStatus::Pass { 0 } else { 1 },
    ))
}

fn trace_probes(args: &ObserveArgs) -> homeboy::core::Result<Vec<TraceProbeConfig>> {
    let mut probes = args
        .probes
        .iter()
        .map(|raw| {
            serde_json::from_str(raw).map_err(|error| {
                Error::validation_invalid_argument(
                    "probe",
                    format!("invalid probe JSON: {error}"),
                    Some(raw.clone()),
                    None,
                )
            })
        })
        .collect::<homeboy::core::Result<Vec<TraceProbeConfig>>>()?;
    probes.extend(args.tail_logs.iter().map(|path| TraceProbeConfig::LogTail {
        path: path.to_string_lossy().to_string(),
        grep: args.grep.clone(),
        match_pattern: None,
    }));
    probes.extend(
        args.watch_processes
            .iter()
            .map(|pattern| TraceProbeConfig::ProcessSnapshot {
                pattern: pattern.clone(),
                interval_ms: Some(duration_millis(args.watch_process_interval)),
            }),
    );
    if probes.is_empty() {
        return Err(Error::validation_invalid_argument(
            "probe",
            "observe requires at least one --tail-log, --watch-process, or --probe",
            None,
            None,
        ));
    }
    Ok(probes)
}

fn error_results(component_id: String, failure: String) -> TraceResults {
    TraceResults {
        component_id,
        scenario_id: "observe".to_string(),
        status: homeboy_extension::trace::TraceStatus::Error,
        summary: Some("Passive trace timeline".to_string()),
        failure: Some(failure),
        rig: None,
        evidence: None,
        timeline: Vec::new(),
        span_definitions: Vec::new(),
        span_results: Vec::new(),
        assertions: Vec::new(),
        temporal_assertions: Vec::new(),
        artifacts: Vec::new(),
        metrics: Default::default(),
        toolchain: None,
        components: None,
        dependencies: Vec::new(),
        preview: None,
    }
}

fn write_trace_results(path: &Path, results: &TraceResults) -> homeboy::core::Result<()> {
    let content = serde_json::to_string_pretty(results).map_err(|error| {
        Error::internal_unexpected(format!("Failed to serialize passive trace: {error}"))
    })?;
    write_output_file_atomically(path, content, OutputWriteOptions::file()).map_err(|error| {
        Error::internal_io(
            format!("Failed to write passive trace {}: {error}", path.display()),
            Some("trace.passive.write_results".to_string()),
        )
    })
}

fn observe_command(args: &ObserveArgs) -> String {
    let mut parts = vec!["homeboy".to_string(), "observe".to_string()];
    if let Some(component) = &args.comp.component {
        parts.push(component.clone());
    }
    if let Some(path) = &args.comp.path {
        parts.extend(["--path".to_string(), path.clone()]);
    }
    parts.extend(["--duration".to_string(), format_duration(args.duration)]);
    for path in &args.tail_logs {
        parts.extend(["--tail-log".to_string(), path.to_string_lossy().to_string()]);
    }
    if let Some(grep) = &args.grep {
        parts.extend(["--grep".to_string(), grep.clone()]);
    }
    for pattern in &args.watch_processes {
        parts.extend(["--watch-process".to_string(), pattern.clone()]);
    }
    for probe in &args.probes {
        parts.extend(["--probe".to_string(), probe.clone()]);
    }
    parts.join(" ")
}

fn parse_duration(raw: &str) -> Result<Duration, String> {
    let split = raw
        .trim()
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "expected duration like 500ms, 30s, 5m, or 1h".to_string())?;
    let (amount, unit) = raw.trim().split_at(split);
    let amount = amount
        .parse::<u64>()
        .map_err(|_| "duration amount must be a positive integer".to_string())?;
    if amount == 0 {
        return Err("duration amount must be greater than zero".to_string());
    }
    match unit {
        "ms" => Ok(Duration::from_millis(amount)),
        "s" => Ok(Duration::from_secs(amount)),
        "m" => Ok(Duration::from_secs(amount * 60)),
        "h" => Ok(Duration::from_secs(amount * 60 * 60)),
        _ => Err("duration unit must be one of ms, s, m, or h".to_string()),
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_millis() < 1000 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{}s", duration.as_secs())
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::test_support::with_isolated_home;

    #[test]
    fn legacy_flags_translate_to_typed_trace_probes() {
        let args = ObserveArgs {
            comp: PositionalComponentArgs {
                component: Some("homeboy".to_string()),
                path: None,
            },
            duration: Duration::from_millis(1),
            tail_logs: vec![PathBuf::from("/tmp/app.log")],
            grep: Some("error".to_string()),
            watch_processes: vec!["homeboy".to_string()],
            watch_process_interval: Duration::from_millis(50),
            probes: Vec::new(),
        };
        let probes = trace_probes(&args).expect("translate probes");
        assert!(matches!(probes[0], TraceProbeConfig::LogTail { .. }));
        assert!(matches!(
            probes[1],
            TraceProbeConfig::ProcessSnapshot {
                interval_ms: Some(50),
                ..
            }
        ));
    }

    #[test]
    fn observe_persists_the_trace_contract_artifact() {
        with_isolated_home(|home| {
            let target = home.path().join("target");
            std::fs::create_dir_all(&target).expect("target dir");
            let args = ObserveArgs {
                comp: PositionalComponentArgs {
                    component: None,
                    path: Some(target.to_string_lossy().to_string()),
                },
                duration: Duration::from_millis(5),
                tail_logs: Vec::new(),
                grep: None,
                watch_processes: vec!["unlikely-homeboy-observe-test-process".to_string()],
                watch_process_interval: Duration::from_millis(1),
                probes: Vec::new(),
            };
            let (output, code) = run(args, &GlobalArgs {}).expect("observe run");
            assert_eq!(code, 0);
            let artifact = std::fs::read_to_string(&output.artifact_path).expect("trace artifact");
            assert!(artifact.contains("trace.passive"));
        });
    }
}
