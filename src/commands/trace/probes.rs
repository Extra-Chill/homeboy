use homeboy::core::extension::trace as extension_trace;
use homeboy::core::rig;

use super::{trace_scenario, trace_workload_scenario_id, TraceArgs, TraceRigContext};

pub(super) fn trace_probes_for_args(
    args: &TraceArgs,
    rig_context: Option<&TraceRigContext>,
    extension_id: Option<&str>,
) -> homeboy::core::Result<Vec<extension_trace::TraceProbeConfig>> {
    let Some(context) = rig_context else {
        return Ok(Vec::new());
    };
    let Some(extension_id) = extension_id else {
        return Ok(Vec::new());
    };
    let scenario = trace_scenario(args)?;
    let workloads = context
        .rig_spec
        .trace_workloads
        .get(extension_id)
        .map(|workloads| workloads.as_slice())
        .unwrap_or(&[]);
    let Some(workload) = workloads
        .iter()
        .find(|workload| trace_workload_scenario_id(workload.path()) == scenario)
    else {
        return Ok(Vec::new());
    };

    workload
        .trace_probes()
        .iter()
        .map(|probe| expand_trace_probe(context, probe))
        .collect()
}

fn expand_trace_probe(
    context: &TraceRigContext,
    probe: &extension_trace::TraceProbeConfig,
) -> homeboy::core::Result<extension_trace::TraceProbeConfig> {
    Ok(match probe {
        extension_trace::TraceProbeConfig::LogTail {
            path,
            grep,
            match_pattern,
        } => extension_trace::TraceProbeConfig::LogTail {
            path: expand_trace_probe_value(context, path),
            grep: grep.clone(),
            match_pattern: match_pattern.clone(),
        },
        extension_trace::TraceProbeConfig::ProcessSnapshot {
            pattern,
            interval_ms,
        } => extension_trace::TraceProbeConfig::ProcessSnapshot {
            pattern: expand_trace_probe_value(context, pattern),
            interval_ms: *interval_ms,
        },
        extension_trace::TraceProbeConfig::FileWatch { path, interval_ms } => {
            extension_trace::TraceProbeConfig::FileWatch {
                path: expand_trace_probe_value(context, path),
                interval_ms: *interval_ms,
            }
        }
        extension_trace::TraceProbeConfig::PortSnapshot {
            port,
            port_range,
            interval_ms,
        } => extension_trace::TraceProbeConfig::PortSnapshot {
            port: *port,
            port_range: port_range.clone(),
            interval_ms: *interval_ms,
        },
        extension_trace::TraceProbeConfig::HttpPoll {
            url,
            interval_ms,
            assert_status,
        } => extension_trace::TraceProbeConfig::HttpPoll {
            url: expand_trace_probe_value(context, url),
            interval_ms: *interval_ms,
            assert_status: *assert_status,
        },
        extension_trace::TraceProbeConfig::HttpEgress {
            host,
            path,
            capture,
            max_body_bytes,
            redact_headers,
            listen_port,
        } => extension_trace::TraceProbeConfig::HttpEgress {
            host: expand_trace_probe_value(context, host),
            path: path
                .as_ref()
                .map(|path| expand_trace_probe_value(context, path)),
            capture: capture.clone(),
            max_body_bytes: *max_body_bytes,
            redact_headers: redact_headers.clone(),
            listen_port: *listen_port,
        },
        extension_trace::TraceProbeConfig::CmdRun { command, args } => {
            extension_trace::TraceProbeConfig::CmdRun {
                command: expand_trace_probe_value(context, command),
                args: args
                    .iter()
                    .map(|arg| expand_trace_probe_value(context, arg))
                    .collect(),
            }
        }
    })
}

fn expand_trace_probe_value(context: &TraceRigContext, value: &str) -> String {
    let expanded = rig::expand::expand_vars(&context.rig_spec, value);
    match context.rig_package_root.as_ref() {
        Some(root) => expanded.replace("${package.root}", &root.to_string_lossy()),
        None => expanded,
    }
}
