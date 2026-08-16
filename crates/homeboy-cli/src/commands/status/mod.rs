//! `homeboy status` — component/project release-state overview.
//!
//! This module is split into focused submodules:
//! - [`types`] — CLI args and serialized output shapes.
//! - [`git_cache`] — per-component git caching and probes.
//! - [`context_paths`] — registered-context detection for the default view.
//! - [`dashboard_table`] — human-readable dashboard rendering.
//!
//! The orchestration entry points (`run`, dashboard/summary builders) live
//! here and compose those pieces.

use clap::Parser;
use homeboy::core::component;
use homeboy::core::context;
use homeboy::core::daemon;
use homeboy::core::observation::{ObservationStore, RunListFilter};
use homeboy::core::project;
use homeboy::core::scope::{self, Scope};
use homeboy::runner::runners as runner;
use homeboy_deploy::ReleaseStateStatus;
use homeboy_engine_primitives::command::{
    wait_with_bounded_output_supervised_with_progress, ControllerChildGuard,
    SupervisedCommandTermination,
};
use homeboy_release::release::version;
use homeboy_upgrade::controller_staleness::{self, ControllerStaleness};
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::CmdResult;

mod context_paths;
mod dashboard_table;
mod git_cache;
mod types;

use dashboard_table::log_dashboard_table;
use git_cache::{fetch_project_remote_versions, log_unreleased_merges, StatusGitCache};

pub use types::{
    CompactContextStatus, CompactStatusOutput, GlobalActivityStatus, GlobalDaemonStatus,
    GlobalInventoryStatus, GlobalRunnerStatus, GlobalStatusOutput, IsolatedStatusFallback,
    ProjectComponentDashboardStatus, ProjectDashboardOutput, ProjectDashboardSummary,
    ProjectStatusRow, StatusArgs, StatusOutput, StatusPartial, StatusPartialComponent,
    StatusResult, StatusTiming, UnregisteredContextStatusOutput, UnregisteredControlPlaneStatus,
    UnreleasedMerge, UpstreamDrift,
};
use types::{
    StatusProbeRequest, StatusProgress, StatusTimer, READY_TO_DEPLOY_NOTE, UNRELEASED_MERGES_NOTE,
};

const STATUS_PROBE_CAPTURE_LIMIT: usize = 1024 * 1024;
const STATUS_PROBE_HEARTBEAT: Duration = Duration::from_secs(1);

pub fn run(args: StatusArgs) -> CmdResult<StatusResult> {
    if args.full && (requires_component_enrichment(&args) || args.refresh) {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "status --full",
            "--full is a context report and cannot apply component filters or refresh Git refs",
            None,
            Some(vec![
                "Use `homeboy status --all <filter>` for component inspection.".to_string(),
                "Use `homeboy status --component <id> --refresh` to refresh one component."
                    .to_string(),
            ]),
        ));
    }
    if args.target.is_none()
        && args.scope.selection().is_none()
        && !args.full
        && !args.all
        && requires_unscoped_enrichment(&args)
    {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "status scope",
            "component filters and --refresh require an explicit enrichment scope",
            None,
            Some(vec![
                "Use `homeboy status --all <filter>` to inspect every registered component."
                    .to_string(),
                "Use `homeboy status --component <id> <filter>` to inspect one component."
                    .to_string(),
            ]),
        ));
    }

    // Refreshing remote refs changes Git metadata, so serialize it with other
    // runtime mutations. Default status is intentionally snapshot-only.
    let _refresh_lease = args
        .refresh
        .then(|| homeboy::core::runtime_promotion::acquire("status refresh", "status"))
        .transpose()?;

    // Git and remote inspection share one deadline. Registry/context resolution
    // is an explicit enrichment operation and currently has no cancellation API.
    let mut timer = StatusTimer::new(args.timings);

    // Every component freshness signal below takes this binary as its reference
    // point, so resolve the reference's own freshness first and say so. Read
    // once from the daily update-check cache — no network, no per-path
    // re-resolution (#11483).
    timer.begin("read_controller_cache");
    let controller = controller_staleness::current();
    timer.finish("read_controller_cache");
    log_controller_staleness(&controller);

    // Context reports and registry resolution can block in filesystem or
    // inventory providers that do not expose cancellation. Put that boundary in
    // a separately killable process while retaining the parent's controller
    // observation for a useful partial result.
    if requires_isolated_probe(&args) {
        return run_isolated_probe(&args, controller, timer);
    }

    run_unisolated(args, controller, timer)
}

fn run_unisolated(
    args: StatusArgs,
    controller: ControllerStaleness,
    mut timer: StatusTimer,
) -> CmdResult<StatusResult> {
    if args.global {
        return global_status(controller);
    }

    // Explicit scope selection. `--path` and `--project` keep their historical
    // routes (checkout inspection and the project dashboard); the remaining
    // selectors resolve through the shared scope resolver into the same
    // component summary the default view builds.
    match args.scope.selection() {
        Some(Scope::Path { .. }) => return run_path_status(&args, controller, timer),
        Some(Scope::Project(ref project_id)) => {
            return run_project_dashboard(project_id, &args, controller, timer)
        }
        Some(selected) => {
            let components = scope::resolve_scope_component_records(&selected)?;
            return summarize_components(components, &args, timer, controller);
        }
        None => {}
    }

    // Project dashboard mode: `homeboy status <project-id>`
    if let Some(ref project_id) = args.target {
        return run_project_dashboard(project_id, &args, controller, timer);
    }

    // Context detection walks the configured inventory and can block on a
    // mounted or unavailable filesystem. Keep that enrichment explicit so the
    // ordinary diagnostic command has no unbounded inventory traversal.
    if !args.full && !args.all {
        timer.begin("build_compact_snapshot");
        let cwd = std::env::current_dir()
            .map_err(|error| homeboy::core::Error::internal_io(error.to_string(), None))?;
        timer.finish("build_compact_snapshot");
        return Ok((
            StatusResult::Compact(CompactStatusOutput {
                command: "status",
                status: "compact",
                cwd: cwd.to_string_lossy().to_string(),
                controller,
                context: CompactContextStatus {
                    status: "not_checked",
                    detail: "CWD, Git, registry, runner, and control-plane inventory were not inspected.",
                    command: "homeboy status --full",
                },
                action: "Run `homeboy status --full` for context/inventory enrichment, `homeboy status --all` to inspect every configured component, or `homeboy status --global` for local control-plane health.",
            }),
            0,
        ));
    }

    if args.full {
        timer.begin("build_full_context");
        let mut report = context::build_report(args.all, "status")?;
        timer.finish("build_full_context");
        report.command = "status".to_string();
        return Ok((StatusResult::Full(Box::new(report)), 0));
    }

    timer.begin("resolve_context");
    let (context_output, all_components, _) = context::run_with_inventory(None)?;
    timer.finish("resolve_context");

    let relevant_ids: std::collections::HashSet<String> = context_output
        .matched_components
        .iter()
        .chain(context_output.contained_components.iter())
        .cloned()
        .collect();

    if relevant_ids.is_empty() && !args.all {
        return Ok((
            StatusResult::UnregisteredContext(UnregisteredContextStatusOutput {
                command: "status",
                status: "unregistered_context",
                cwd: context_output.cwd,
                git_root: context_output.git_root,
                suggestion: context_output.suggestion.unwrap_or_else(|| {
                    "Repo not attached. Prefer: `homeboy project components attach-path <project-id> <path>`"
                        .to_string()
                }),
                action: "Run `homeboy status --global` for bounded local runner/control-plane health, `homeboy status --all` to inspect every configured component, or attach this checkout to a project/component first.",
                control_plane: UnregisteredControlPlaneStatus {
                    status: "not_checked",
                    command: "homeboy status --global",
                },
            }),
            0,
        ));
    }

    let show_all = args.all || relevant_ids.is_empty();

    let components: Vec<component::Component> = if show_all {
        all_components
    } else {
        all_components
            .into_iter()
            .filter(|c| relevant_ids.contains(&c.id))
            .collect()
    };

    summarize_components(components, &args, timer, controller)
}

fn requires_isolated_probe(args: &StatusArgs) -> bool {
    // Library unit tests call command handlers directly, so their test harness
    // cannot service the binary-private child entry point. End-to-end binary
    // tests exercise the containment boundary itself.
    #[cfg(test)]
    {
        let _ = args;
        false
    }
    #[cfg(not(test))]
    {
        args.full
            || args.all
            || matches!(args.scope.selection(), Some(scope) if !matches!(scope, Scope::Path { .. }))
    }
}

fn run_isolated_probe(
    args: &StatusArgs,
    controller: ControllerStaleness,
    mut timer: StatusTimer,
) -> CmdResult<StatusResult> {
    const PHASE: &str = "probe_context_inventory_scope";
    timer.begin(PHASE);
    let argv = status_probe_argv(args);
    let request = serde_json::to_string(&StatusProbeRequest { argv })
        .map_err(|error| homeboy::core::Error::internal_unexpected(error.to_string()))?;
    let executable = std::env::current_exe()
        .map_err(|error| homeboy::core::Error::internal_io(error.to_string(), None))?;
    let mut command = Command::new(executable);
    command
        .arg("__homeboy_status_probe")
        .arg(request)
        .env("HOMEBOY_STATUS_PROBE_CHILD", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let guard = ControllerChildGuard::prepare(&mut command)
        .map_err(|error| homeboy::core::Error::internal_io(error.to_string(), None))?;
    let mut child = command
        .spawn()
        .map_err(|error| homeboy::core::Error::internal_io(error.to_string(), None))?;
    guard
        .attach(&child)
        .map_err(|error| homeboy::core::Error::internal_io(error.to_string(), None))?;
    let remaining = timer.remaining().unwrap_or(Duration::ZERO);
    let supervised = wait_with_bounded_output_supervised_with_progress(
        &mut child,
        STATUS_PROBE_CAPTURE_LIMIT,
        remaining,
        None,
        STATUS_PROBE_HEARTBEAT,
        || false,
        |heartbeat| {
            if let Some(progress) = heartbeat.progress {
                eprintln!("[status] {PHASE}: {}", progress.phase);
            } else {
                eprintln!(
                    "[status] {PHASE}: waiting ({}ms)",
                    heartbeat.elapsed.as_millis()
                );
            }
            Ok(())
        },
    );
    timer.finish(PHASE);
    let fallback = |reason, diagnostic| {
        Ok((
            StatusResult::ProbeFallback(IsolatedStatusFallback {
                command: "status",
                status: "partial",
                controller,
                partial: StatusPartial {
                    reason,
                    phase: PHASE,
                    omitted_components: Vec::new(),
                    degraded_components: Vec::new(),
                    degraded_component_phases: Vec::new(),
                    replay_commands: vec![status_probe_replay(args)],
                },
                diagnostic,
            }),
            0,
        ))
    };
    let supervised = match supervised {
        Ok(output) => output,
        Err(error) => return fallback("status_probe_supervision_failed", error.to_string()),
    };
    let termination_reason = match supervised.termination {
        SupervisedCommandTermination::Completed if supervised.output.status.success() => None,
        SupervisedCommandTermination::Completed => Some("status_probe_failed"),
        SupervisedCommandTermination::TimedOut => Some("status_probe_timeout"),
        SupervisedCommandTermination::NoProgress => Some("status_probe_stalled"),
        SupervisedCommandTermination::Cancelled => Some("status_probe_cancelled"),
    };
    if let Some(reason) = termination_reason {
        return fallback(
            reason,
            String::from_utf8_lossy(&supervised.output.stderr)
                .trim()
                .to_string(),
        );
    }
    let result = match serde_json::from_slice(&supervised.output.stdout) {
        Ok(result) => result,
        Err(error) => return fallback("status_probe_invalid_output", error.to_string()),
    };
    Ok((StatusResult::Isolated(result), 0))
}

fn status_probe_argv(args: &StatusArgs) -> Vec<String> {
    let mut argv = vec!["homeboy".to_string(), "status".to_string()];
    if let Some(target) = &args.target {
        argv.push(target.clone());
    }
    for (flag, value) in [
        ("--project", args.scope.project.as_ref()),
        ("--fleet", args.scope.fleet.as_ref()),
        ("--component", args.scope.component.as_ref()),
        ("--rig", args.scope.rig.as_ref()),
        ("--path", args.scope.path.as_ref()),
    ] {
        if let Some(value) = value {
            argv.extend([flag.to_string(), value.clone()]);
        }
    }
    if args.scope.workspace {
        argv.push("--workspace".to_string());
    }
    for (enabled, flag) in [
        (args.full, "--full"),
        (args.uncommitted, "--uncommitted"),
        (args.needs_release, "--needs-release"),
        (args.ready, "--ready"),
        (args.docs_only, "--docs-only"),
        (args.all, "--all"),
        (args.outdated, "--outdated"),
        (args.timings, "--timings"),
        (args.refresh, "--refresh"),
        (args.unreleased, "--unreleased"),
    ] {
        if enabled {
            argv.push(flag.to_string());
        }
    }
    argv
}

fn status_probe_replay(args: &StatusArgs) -> String {
    status_probe_argv(args).join(" ")
}

/// Entry point used only by the private same-binary child protocol.
pub fn run_status_probe_child(request: Option<&String>) -> std::process::ExitCode {
    let Some(request) = request else {
        return std::process::ExitCode::from(2);
    };
    let Ok(request) = serde_json::from_str::<StatusProbeRequest>(request) else {
        return std::process::ExitCode::from(2);
    };
    let Ok(cli) = crate::cli_surface::Cli::try_parse_from(request.argv) else {
        return std::process::ExitCode::from(2);
    };
    let crate::cli_surface::Commands::Status(args) = cli.command else {
        return std::process::ExitCode::from(2);
    };
    let mut timer = StatusTimer::new(args.timings);
    timer.begin("child_probe_start");
    let controller = controller_staleness::current();
    timer.finish("child_probe_start");
    match run_unisolated(args, controller, timer) {
        Ok((result, code)) => match serde_json::to_writer(std::io::stdout(), &result) {
            Ok(()) => std::process::ExitCode::from(code as u8),
            Err(_) => std::process::ExitCode::from(2),
        },
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::from(2)
        }
    }
}

fn requires_unscoped_enrichment(args: &StatusArgs) -> bool {
    requires_component_enrichment(args) || args.refresh
}

fn requires_component_enrichment(args: &StatusArgs) -> bool {
    args.uncommitted || args.needs_release || args.ready || args.docs_only || args.unreleased
}

const GLOBAL_RUNNER_LIMIT: usize = 64;
const GLOBAL_ACTIVITY_LIMIT: i64 = 100;

/// Read the controller's own local stores without resolving the caller's CWD,
/// fetching component remotes, or probing runner daemons. Counts are capped at
/// their query boundary and runner inspection is capped independently of the
/// registered inventory size.
fn global_status(controller: ControllerStaleness) -> CmdResult<StatusResult> {
    let daemon_status = daemon::read_status()?;
    let admitting_work = daemon_status.running && daemon_status.fresh && daemon_status.reachable;
    let blocker = (!admitting_work).then(|| {
        daemon_status
            .stale_reason
            .clone()
            .unwrap_or_else(|| "daemon is not running, fresh, and reachable".to_string())
    });

    let registered_runners = runner::list()?;
    let inspected_runners = registered_runners.len().min(GLOBAL_RUNNER_LIMIT);
    let mut status_unavailable = 0;
    let runner_reports = registered_runners
        .iter()
        .take(GLOBAL_RUNNER_LIMIT)
        .filter_map(
            |runner_config| match runner::persisted_status(&runner_config.id) {
                Ok(report) => Some(report),
                Err(_) => {
                    status_unavailable += 1;
                    None
                }
            },
        )
        .collect::<Vec<_>>();
    let disconnected = runner_reports
        .iter()
        .filter(|report| !report.connected)
        .count();

    let activity = ObservationStore::open_readonly()
        .ok()
        .map(|store| {
            let active = store
                .list_active_runs_bounded(GLOBAL_ACTIVITY_LIMIT + 1)
                .unwrap_or_default();
            let recent = store
                .list_runs_page(RunListFilter {
                    limit: Some(GLOBAL_ACTIVITY_LIMIT),
                    ..Default::default()
                })
                .ok();
            GlobalActivityStatus {
                active_truncated: active.len() > GLOBAL_ACTIVITY_LIMIT as usize,
                active: active.len().min(GLOBAL_ACTIVITY_LIMIT as usize),
                recent: recent.as_ref().map_or(0, |page| page.runs.len()),
                recent_truncated: recent.is_some_and(|page| page.truncated),
                drill_down: "homeboy activity",
            }
        })
        .unwrap_or(GlobalActivityStatus {
            active: 0,
            active_truncated: false,
            recent: 0,
            recent_truncated: false,
            drill_down: "homeboy activity",
        });

    Ok((
        StatusResult::Global(GlobalStatusOutput {
            command: "status",
            status: "global",
            controller,
            daemon: GlobalDaemonStatus {
                admitting_work,
                fresh: daemon_status.fresh,
                reachable: daemon_status.reachable,
                active_jobs: daemon_status.freshness.active_jobs,
                blocker,
                drill_down: "homeboy daemon status",
                repair: (!admitting_work).then_some("homeboy daemon recover"),
            },
            runners: GlobalRunnerStatus {
                registered: registered_runners.len(),
                inspected: inspected_runners,
                omitted: registered_runners.len().saturating_sub(inspected_runners),
                disconnected,
                status_unavailable,
                freshness_unverified: inspected_runners,
                drill_down: "homeboy runner status --full",
            },
            activity,
            inventory: GlobalInventoryStatus {
                projects: project::list().unwrap_or_default().len(),
                components: component::registered().unwrap_or_default().len(),
            },
            drill_down: vec![
                "homeboy daemon status",
                "homeboy runner status --full",
                "homeboy activity",
                "homeboy runs list --limit 100",
                "homeboy component list",
            ],
        }),
        0,
    ))
}

/// Emit the controller-staleness warning to the status channel.
///
/// Reporting only, never fatal: a stale controller still runs the command it
/// was asked to run. `ControllerStaleness` yields a line only when staleness is
/// *established*, so an offline host or a disabled update check stays silent
/// rather than warning on a verdict it does not have.
fn log_controller_staleness(controller: &ControllerStaleness) {
    if let Some(line) = controller.warning_line() {
        homeboy::log_status!("status", "{}", line);
    }
}

fn summarize_components(
    components: Vec<component::Component>,
    args: &StatusArgs,
    mut timer: StatusTimer,
    controller: ControllerStaleness,
) -> CmdResult<StatusResult> {
    let total = components.len();
    let mut observations = StatusObservations::default();
    let mut git_cache = StatusGitCache::with_refresh(args.refresh);

    let has_filter =
        args.uncommitted || args.needs_release || args.ready || args.docs_only || args.unreleased;
    let include_upstream_drift = !has_filter;
    let include_unreleased_merges = !has_filter || args.unreleased;

    if include_upstream_drift || include_unreleased_merges {
        timer.begin("inspect_upstream_and_unreleased");
        // This phase runs a per-component `git fetch` and drift/merge probes. In
        // a large workspace it is the slowest part of `status`, so emit a bounded
        // progress line per component (to stderr, on a TTY) so operators see what
        // it is working on instead of a silent hang (#7378).
        let progress = StatusProgress::new(total);
        for (index, comp) in components.iter().enumerate() {
            if timer.expired() {
                timer.finish("inspect_upstream_and_unreleased");
                return partial_status(
                    components,
                    index,
                    git_cache.degraded_components,
                    git_cache.degraded_component_phases,
                    timer,
                    controller,
                    observations,
                    "inspect_upstream_and_unreleased",
                    args,
                );
            }
            progress.report(index, &comp.id);
            if include_upstream_drift {
                if let Some(drift) = git_cache.fetch_upstream_drift_for(comp, &timer) {
                    if drift.is_behind() {
                        observations.behind_upstream.push(comp.id.clone());
                    }
                    observations.upstream_drift.push(drift);
                }
            } else if include_unreleased_merges {
                git_cache.fetch_origin_tags_for(&comp.local_path, &timer);
            }

            // Detect merged-but-unreleased work per component (issue #4996). This is
            // measured against origin/<default-branch> (refreshed above), so a stale
            // local checkout does not hide unreleased merges.
            if include_unreleased_merges {
                if let Some(merge) = git_cache.detect_unreleased_merges_for(comp, &timer) {
                    observations.unreleased_merges.push(merge);
                }
            }
        }
        timer.finish("inspect_upstream_and_unreleased");
    }

    timer.begin("inspect_release_state");
    for (index, comp) in components.iter().enumerate() {
        if timer.expired() {
            timer.finish("inspect_release_state");
            return partial_status(
                components,
                index,
                git_cache.degraded_components,
                git_cache.degraded_component_phases,
                timer,
                controller,
                observations,
                "inspect_release_state",
                args,
            );
        }
        let status = git_cache
            .release_state_for(comp, &timer)
            .map(|state| state.status())
            .unwrap_or(ReleaseStateStatus::Unknown);

        match status {
            ReleaseStateStatus::Uncommitted => observations.uncommitted.push(comp.id.clone()),
            ReleaseStateStatus::NeedsRelease => observations.needs_release.push(comp.id.clone()),
            ReleaseStateStatus::DocsOnly => observations.docs_only.push(comp.id.clone()),
            ReleaseStateStatus::Clean => observations.ready_to_deploy.push(comp.id.clone()),
            ReleaseStateStatus::Unknown => observations.clean += 1,
        }
    }
    timer.finish("inspect_release_state");

    // Apply filters if any are set
    if has_filter {
        if !args.uncommitted {
            observations.uncommitted.clear();
        }
        if !args.needs_release {
            observations.needs_release.clear();
        }
        if !args.ready {
            observations.ready_to_deploy.clear();
        }
        if !args.docs_only {
            observations.docs_only.clear();
        }
        if !args.unreleased {
            observations.unreleased_merges.clear();
        }
    }

    let ready_to_deploy_note = if observations.ready_to_deploy.is_empty() {
        None
    } else {
        Some(READY_TO_DEPLOY_NOTE)
    };

    let unreleased_merges_note = if observations.unreleased_merges.is_empty() {
        None
    } else {
        Some(UNRELEASED_MERGES_NOTE)
    };

    log_unreleased_merges(&observations.unreleased_merges);

    Ok((
        StatusResult::Summary(StatusOutput {
            command: "status",
            total,
            uncommitted: observations.uncommitted,
            needs_release: observations.needs_release,
            ready_to_deploy: observations.ready_to_deploy,
            ready_to_deploy_note,
            docs_only: observations.docs_only,
            behind_upstream: observations.behind_upstream,
            upstream_drift: observations.upstream_drift,
            unreleased_merges: observations.unreleased_merges,
            unreleased_merges_note,
            timings: timer.into_timings(),
            clean: observations.clean,
            partial: (!git_cache.degraded_components.is_empty()).then(|| StatusPartial {
                reason: "component_git_probe_degraded",
                phase: "component_git_probe",
                omitted_components: Vec::new(),
                replay_commands: replay_component_commands(&git_cache.degraded_components, args),
                degraded_components: sorted_component_ids(git_cache.degraded_components),
                degraded_component_phases: sorted_degraded_component_phases(
                    git_cache.degraded_component_phases,
                ),
            }),
            controller,
        }),
        0,
    ))
}

fn sorted_degraded_component_phases(
    phases: std::collections::HashMap<String, std::collections::HashSet<&'static str>>,
) -> Vec<StatusPartialComponent> {
    let mut phases: Vec<_> = phases
        .into_iter()
        .map(|(component_id, phases)| {
            let mut phases: Vec<_> = phases.into_iter().collect();
            phases.sort();
            StatusPartialComponent {
                component_id,
                phases,
            }
        })
        .collect();
    phases.sort_by(|left, right| left.component_id.cmp(&right.component_id));
    phases
}

fn project_degraded_component_phases(
    mut phases: std::collections::HashMap<String, std::collections::HashSet<&'static str>>,
    remote_diagnostics: &HashMap<String, String>,
) -> Vec<StatusPartialComponent> {
    for component_id in remote_diagnostics.keys() {
        phases
            .entry(component_id.clone())
            .or_default()
            .insert("fetch_remote_versions");
    }
    sorted_degraded_component_phases(phases)
}

fn sorted_component_ids(ids: std::collections::HashSet<String>) -> Vec<String> {
    let mut ids: Vec<_> = ids.into_iter().collect();
    ids.sort();
    ids
}

fn replay_component_commands(
    ids: &std::collections::HashSet<String>,
    args: &StatusArgs,
) -> Vec<String> {
    let mut ids: Vec<_> = ids.iter().collect();
    ids.sort();
    ids.into_iter()
        .map(|id| {
            format!(
                "homeboy status --component {id}{}",
                replay_status_flags(args)
            )
        })
        .collect()
}

fn replay_status_flags(args: &StatusArgs) -> String {
    let mut flags = Vec::new();
    if args.uncommitted {
        flags.push("--uncommitted");
    }
    if args.needs_release {
        flags.push("--needs-release");
    }
    if args.ready {
        flags.push("--ready");
    }
    if args.docs_only {
        flags.push("--docs-only");
    }
    if args.unreleased {
        flags.push("--unreleased");
    }
    if args.outdated {
        flags.push("--outdated");
    }
    if args.refresh {
        flags.push("--refresh");
    }
    flags.push("--timings");
    format!(" {}", flags.join(" "))
}

fn partial_status(
    components: Vec<component::Component>,
    index: usize,
    degraded_components: std::collections::HashSet<String>,
    degraded_component_phases: std::collections::HashMap<
        String,
        std::collections::HashSet<&'static str>,
    >,
    timer: StatusTimer,
    controller: ControllerStaleness,
    observations: StatusObservations,
    phase: &'static str,
    args: &StatusArgs,
) -> CmdResult<StatusResult> {
    let omitted_components: Vec<_> = components[index..]
        .iter()
        .map(|component| component.id.clone())
        .collect();
    let mut replay_commands = replay_component_commands(&degraded_components, args);
    replay_commands.extend(omitted_components.iter().map(|id| {
        format!(
            "homeboy status --component {id}{}",
            replay_status_flags(args)
        )
    }));
    Ok((
        StatusResult::Summary(StatusOutput {
            command: "status",
            total: components.len(),
            uncommitted: observations.uncommitted,
            needs_release: observations.needs_release,
            ready_to_deploy_note: (!observations.ready_to_deploy.is_empty())
                .then_some(READY_TO_DEPLOY_NOTE),
            ready_to_deploy: observations.ready_to_deploy,
            docs_only: observations.docs_only,
            behind_upstream: observations.behind_upstream,
            upstream_drift: observations.upstream_drift,
            unreleased_merges_note: (!observations.unreleased_merges.is_empty())
                .then_some(UNRELEASED_MERGES_NOTE),
            unreleased_merges: observations.unreleased_merges,
            timings: timer.into_timings(),
            clean: observations.clean,
            partial: Some(StatusPartial {
                reason: "total_latency_budget_exhausted",
                phase,
                omitted_components,
                degraded_components: sorted_component_ids(degraded_components),
                degraded_component_phases: sorted_degraded_component_phases(
                    degraded_component_phases,
                ),
                replay_commands,
            }),
            controller,
        }),
        0,
    ))
}

#[derive(Default)]
struct StatusObservations {
    uncommitted: Vec<String>,
    needs_release: Vec<String>,
    ready_to_deploy: Vec<String>,
    docs_only: Vec<String>,
    behind_upstream: Vec<String>,
    upstream_drift: Vec<UpstreamDrift>,
    unreleased_merges: Vec<UnreleasedMerge>,
    clean: usize,
}

/// Path override mode: inspect one checkout without requiring registry membership.
fn run_path_status(
    args: &StatusArgs,
    controller: ControllerStaleness,
    mut timer: StatusTimer,
) -> CmdResult<StatusResult> {
    let path = args.scope.path.as_deref();
    timer.begin("resolve_path_component");
    let mut component = component::resolve_effective(args.target.as_deref(), path, None)?;
    if let Some(path) = path {
        // An explicit ID gives this report its stable identity; `--path` is the
        // checkout override and must remain the component's inspected source.
        component.local_path = path.to_string();
    }
    timer.finish("resolve_path_component");

    if args.full {
        let component_id = component.id.clone();
        let component_path = component.local_path.clone();
        let mut report = context::build_report_for_component(args.all, "status", component, path)?;
        report.command = "status".to_string();
        // The focused component may have been resolved through an explicit ID,
        // but `--path` remains the context the operator asked to inspect.
        if let Some(path) = path {
            report.context.cwd = path.to_string();
        }
        if let Some(summary) = report
            .components
            .iter_mut()
            .find(|summary| summary.id == component_id)
        {
            summary.path = component_path;
        }
        return Ok((StatusResult::Full(Box::new(report)), 0));
    }

    summarize_components(vec![component], args, timer, controller)
}

/// Project dashboard: show version drift across all components in a project.
///
/// Combines local version, remote (deployed) version, release state, upstream
/// drift, and unreleased commit count into a single view per component.
fn run_project_dashboard(
    project_id: &str,
    args: &StatusArgs,
    controller: ControllerStaleness,
    mut timer: StatusTimer,
) -> CmdResult<StatusResult> {
    timer.begin("resolve_project_components");
    let components = scope::resolve_scope_component_records(&Scope::Project(project_id.into()))?;
    timer.finish("resolve_project_components");

    if components.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "project",
            format!("Project '{}' has no components attached", project_id),
            Some(project_id.to_string()),
            Some(vec![
                "Attach components with: homeboy project set <project> --json '{\"components\":[{\"id\":\"...\",\"local_path\":\"...\"}]}'".to_string(),
            ]),
        ));
    }

    // Gather local versions
    timer.begin("read_local_versions");
    let local_versions: std::collections::HashMap<String, String> = components
        .iter()
        .filter_map(|c| version::get_component_version(c).map(|v| (c.id.clone(), v)))
        .collect();
    timer.finish("read_local_versions");

    // Gather remote versions via deploy check mode (handles SSH internally)
    timer.begin("fetch_remote_versions");
    let remote_probe = fetch_project_remote_versions(project_id, &components, &timer);
    let remote_versions = remote_probe.result.versions;
    let mut remote_diagnostics: HashMap<String, String> = remote_probe
        .result
        .failures
        .into_iter()
        .map(|failure| (failure.component_id, failure.diagnostic))
        .collect();
    if let Some(failure) = remote_probe.failure {
        for component in &components {
            remote_diagnostics
                .entry(component.id.clone())
                .or_insert_with(|| failure.clone());
        }
    }
    timer.finish("fetch_remote_versions");

    if timer.expired() {
        return Ok((
            StatusResult::Dashboard(ProjectDashboardOutput {
                command: "status",
                project_id: project_id.to_string(),
                total: 0,
                components: Vec::new(),
                summary: ProjectDashboardSummary {
                    current: 0,
                    pinned_current: 0,
                    outdated: 0,
                    needs_release: 0,
                    docs_only: 0,
                    uncommitted: 0,
                    behind_upstream: 0,
                    bundled: 0,
                    retired: 0,
                    unknown: 0,
                    degraded: 0,
                },
                timings: timer.into_timings(),
                partial: Some(StatusPartial {
                    reason: "total_latency_budget_exhausted",
                    phase: "fetch_remote_versions",
                    omitted_components: components
                        .iter()
                        .map(|component| component.id.clone())
                        .collect(),
                    degraded_components: Vec::new(),
                    degraded_component_phases: Vec::new(),
                    replay_commands: vec![format!(
                        "homeboy status {project_id}{}",
                        replay_status_flags(args)
                    )],
                }),
                controller,
            }),
            0,
        ));
    }

    let mut git_cache = StatusGitCache::with_refresh(args.refresh);

    // Fetch upstream drift for all components
    timer.begin("inspect_upstream_drift");
    let mut upstream_drift_map = std::collections::HashMap::new();
    let mut omitted_components = Vec::new();
    for (index, component) in components.iter().enumerate() {
        if timer.expired() {
            omitted_components.extend(
                components[index..]
                    .iter()
                    .map(|component| component.id.clone()),
            );
            break;
        }
        if let Some(drift) = git_cache.fetch_upstream_drift_for(component, &timer) {
            upstream_drift_map.insert(component.id.clone(), drift);
        }
    }
    timer.finish("inspect_upstream_drift");

    // Build per-component rows
    let mut rows: Vec<ProjectStatusRow> = Vec::new();
    let mut summary = ProjectDashboardSummary {
        current: 0,
        pinned_current: 0,
        outdated: 0,
        needs_release: 0,
        docs_only: 0,
        uncommitted: 0,
        behind_upstream: 0,
        bundled: 0,
        retired: 0,
        unknown: 0,
        degraded: 0,
    };

    timer.begin("build_dashboard_rows");
    for (index, comp) in components.iter().enumerate() {
        if timer.expired() {
            omitted_components.extend(
                components[index..]
                    .iter()
                    .map(|component| component.id.clone()),
            );
            break;
        }
        // Bundled/retired components are no longer standalone deploy targets.
        // Surface their lifecycle status for visibility, but do not run the
        // version/release-state machinery that would flag false `outdated`
        // drift (issue #3489).
        if !comp.is_active_lifecycle() {
            let dashboard_status = match comp.lifecycle {
                component::ComponentLifecycle::Bundled => {
                    summary.bundled += 1;
                    ProjectComponentDashboardStatus::Bundled
                }
                _ => {
                    summary.retired += 1;
                    ProjectComponentDashboardStatus::Retired
                }
            };
            rows.push(ProjectStatusRow {
                component_id: comp.id.clone(),
                local_version: local_versions.get(&comp.id).cloned(),
                remote_version: None,
                remote_version_diagnostic: None,
                origin_version: None,
                unreleased_commits: 0,
                ahead_upstream: None,
                behind_upstream: None,
                status: dashboard_status,
            });
            continue;
        }

        let local_ver = local_versions.get(&comp.id).cloned();
        let remote_ver = remote_versions.get(&comp.id).cloned();
        let drift = upstream_drift_map.get(&comp.id);

        let release_state = git_cache.release_state_for(comp, &timer).cloned();
        let release_status = release_state
            .as_ref()
            .map(|s| s.status())
            .unwrap_or(ReleaseStateStatus::Unknown);

        let unreleased_commits = release_state
            .as_ref()
            .map(|s| s.commits_since_version)
            .unwrap_or(0);

        // Determine dashboard status.
        // Priority: uncommitted > needs_release > docs_only > behind_upstream > outdated > current > unknown
        let dashboard_status = if remote_diagnostics.contains_key(&comp.id) {
            ProjectComponentDashboardStatus::Degraded
        } else {
            match release_status {
                ReleaseStateStatus::Uncommitted => ProjectComponentDashboardStatus::Uncommitted,
                ReleaseStateStatus::NeedsRelease => ProjectComponentDashboardStatus::NeedsRelease,
                ReleaseStateStatus::DocsOnly => ProjectComponentDashboardStatus::DocsOnly,
                ReleaseStateStatus::Clean => {
                    // Check source freshness first. Deployment health is evaluated
                    // independently, so a newer target is not marked outdated when
                    // this configured checkout is stale.
                    if let Some(d) = drift {
                        if d.is_behind() {
                            ProjectComponentDashboardStatus::BehindUpstream
                        } else {
                            deployed_version_dashboard_status(
                                &local_ver,
                                &remote_ver,
                                d.latest_origin_tag.as_deref(),
                            )
                        }
                    } else {
                        deployed_version_dashboard_status(&local_ver, &remote_ver, None)
                    }
                }
                ReleaseStateStatus::Unknown => ProjectComponentDashboardStatus::Unknown,
            }
        };

        match &dashboard_status {
            ProjectComponentDashboardStatus::Current => summary.current += 1,
            ProjectComponentDashboardStatus::PinnedCurrent => summary.pinned_current += 1,
            ProjectComponentDashboardStatus::Outdated => summary.outdated += 1,
            ProjectComponentDashboardStatus::NeedsRelease => summary.needs_release += 1,
            ProjectComponentDashboardStatus::DocsOnly => summary.docs_only += 1,
            ProjectComponentDashboardStatus::Uncommitted => summary.uncommitted += 1,
            ProjectComponentDashboardStatus::BehindUpstream => summary.behind_upstream += 1,
            // Lifecycle statuses are assigned on the early-return path above and
            // never reach this active-component branch.
            ProjectComponentDashboardStatus::Bundled => summary.bundled += 1,
            ProjectComponentDashboardStatus::Retired => summary.retired += 1,
            ProjectComponentDashboardStatus::Unknown => summary.unknown += 1,
            ProjectComponentDashboardStatus::Degraded => summary.degraded += 1,
        }

        rows.push(ProjectStatusRow {
            component_id: comp.id.clone(),
            local_version: local_ver,
            remote_version: remote_ver,
            remote_version_diagnostic: remote_diagnostics.get(&comp.id).cloned(),
            origin_version: drift.and_then(|d| d.latest_origin_tag.clone()),
            unreleased_commits,
            ahead_upstream: drift.and_then(|d| d.ahead),
            behind_upstream: drift.and_then(|d| d.behind),
            status: dashboard_status,
        });
    }
    timer.finish("build_dashboard_rows");

    // Apply filters
    if args.outdated {
        rows.retain(|r| {
            matches!(
                r.status,
                ProjectComponentDashboardStatus::Outdated
                    | ProjectComponentDashboardStatus::PinnedCurrent
            )
        });
    }
    if args.needs_release {
        rows.retain(|r| matches!(r.status, ProjectComponentDashboardStatus::NeedsRelease));
    }
    if args.uncommitted {
        rows.retain(|r| matches!(r.status, ProjectComponentDashboardStatus::Uncommitted));
    }
    if args.docs_only {
        rows.retain(|r| matches!(r.status, ProjectComponentDashboardStatus::DocsOnly));
    }
    if args.ready {
        rows.retain(|r| matches!(r.status, ProjectComponentDashboardStatus::Current));
    }

    // Log the table to stderr for human-readable output
    log_dashboard_table(&rows);

    let total = rows.len();

    Ok((
        StatusResult::Dashboard(ProjectDashboardOutput {
            command: "status",
            project_id: project_id.to_string(),
            total,
            components: rows,
            summary,
            timings: timer.into_timings(),
            partial: (!omitted_components.is_empty()
                || !git_cache.degraded_components.is_empty()
                || !remote_diagnostics.is_empty())
            .then(|| StatusPartial {
                reason: if omitted_components.is_empty() {
                    "project_probe_degraded"
                } else {
                    "total_latency_budget_exhausted"
                },
                phase: if omitted_components.is_empty() {
                    "project_remote_or_git_probe"
                } else {
                    "project_component_inspection"
                },
                omitted_components,
                degraded_components: sorted_component_ids(
                    git_cache
                        .degraded_components
                        .into_iter()
                        .chain(remote_diagnostics.keys().cloned())
                        .collect(),
                ),
                degraded_component_phases: project_degraded_component_phases(
                    git_cache.degraded_component_phases,
                    &remote_diagnostics,
                ),
                replay_commands: vec![format!(
                    "homeboy status {project_id}{}",
                    replay_status_flags(args)
                )],
            }),
            controller,
        }),
        0,
    ))
}

fn deployed_version_dashboard_status(
    local_ver: &Option<String>,
    remote_ver: &Option<String>,
    origin_tag: Option<&str>,
) -> ProjectComponentDashboardStatus {
    match homeboy_deploy::compare_deployed_versions(local_ver.as_deref(), remote_ver.as_deref()) {
        homeboy_deploy::ComponentStatus::NeedsUpdate => ProjectComponentDashboardStatus::Outdated,
        homeboy_deploy::ComponentStatus::UpToDate
            if local_ver
                .as_deref()
                .is_some_and(|local| origin_tag_is_newer_than_local(origin_tag, local)) =>
        {
            ProjectComponentDashboardStatus::PinnedCurrent
        }
        homeboy_deploy::ComponentStatus::Unknown => ProjectComponentDashboardStatus::Unknown,
        homeboy_deploy::ComponentStatus::UpToDate
        | homeboy_deploy::ComponentStatus::BehindRemote => ProjectComponentDashboardStatus::Current,
        homeboy_deploy::ComponentStatus::BehindUpstream
        | homeboy_deploy::ComponentStatus::SourceStale
        | homeboy_deploy::ComponentStatus::VersionUpToDateContentUnverified
        | homeboy_deploy::ComponentStatus::RemoteModified
        | homeboy_deploy::ComponentStatus::Missing
        | homeboy_deploy::ComponentStatus::MixedDrift => {
            unreachable!("version comparison only returns version statuses")
        }
    }
}

fn origin_tag_is_newer_than_local(origin_tag: Option<&str>, local: &str) -> bool {
    let Some(origin) = origin_tag else {
        return false;
    };
    let origin = origin.trim_start_matches('v');
    let local = local.trim_start_matches('v');
    if origin == local {
        return false;
    }

    semver::Version::parse(origin)
        .ok()
        .zip(semver::Version::parse(local).ok())
        .is_some_and(|(origin, local)| origin > local)
}

#[cfg(test)]
mod tests {
    use super::git_cache::{component_cache_key, default_origin_branch, upstream_drift_cache_key};
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    use tempfile::TempDir;

    static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    static STATUS_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn status_args(project: Option<String>, path: String, full: bool) -> StatusArgs {
        StatusArgs {
            target: project,
            scope: crate::commands::utils::args::ScopeArgs {
                path: Some(path),
                ..Default::default()
            },
            full,
            uncommitted: false,
            needs_release: false,
            ready: false,
            docs_only: false,
            all: false,
            global: false,
            outdated: false,
            unreleased: false,
            timings: false,
            refresh: false,
        }
    }

    fn default_status_args() -> StatusArgs {
        StatusArgs {
            target: None,
            scope: crate::commands::utils::args::ScopeArgs::default(),
            full: false,
            uncommitted: false,
            needs_release: false,
            ready: false,
            docs_only: false,
            all: false,
            global: false,
            outdated: false,
            unreleased: false,
            timings: false,
            refresh: false,
        }
    }

    fn with_hung_git_probe<R>(needle: &str, run: impl FnOnce(std::path::PathBuf) -> R) -> R {
        let _guard = STATUS_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = TempDir::new().expect("fake git fixture");
        let fake_git = fixture.path().join("git");
        let pid_file = fixture.path().join("hung-child.pid");
        let real_git = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("locate git");
        let real_git = String::from_utf8(real_git.stdout)
            .expect("git path")
            .trim()
            .to_string();
        fs::write(
            &fake_git,
            "#!/bin/sh\ncase \"$*\" in *\"$HOMEBOY_STATUS_HANG_GIT\"*) sleep 10 & echo $! > \"$HOMEBOY_STATUS_HANG_PID\"; wait ;; esac\nexec \"$HOMEBOY_STATUS_REAL_GIT\" \"$@\"\n",
        )
        .expect("write fake git");
        #[cfg(unix)]
        fs::set_permissions(
            &fake_git,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("make fake git executable");
        let old_path = env::var("PATH").unwrap_or_default();
        env::set_var("PATH", format!("{}:{old_path}", fixture.path().display()));
        env::set_var("HOMEBOY_STATUS_REAL_GIT", real_git);
        env::set_var("HOMEBOY_STATUS_HANG_GIT", needle);
        env::set_var("HOMEBOY_STATUS_HANG_PID", &pid_file);
        let result = run(pid_file.clone());
        env::set_var("PATH", old_path);
        env::remove_var("HOMEBOY_STATUS_REAL_GIT");
        env::remove_var("HOMEBOY_STATUS_HANG_GIT");
        env::remove_var("HOMEBOY_STATUS_HANG_PID");
        result
    }

    fn assert_hung_probe_is_bounded(
        needle: &str,
        expected_phase: &'static str,
        expected_component: &str,
        require_degraded_component: bool,
    ) {
        with_hung_git_probe(needle, |pid_file| {
            let (_dir, repo) = make_committed_git_repo("bounded-status");
            run_git(&repo, &["tag", "v0.1.0"]);
            let component = component::Component {
                id: expected_component.to_string(),
                local_path: repo.to_string_lossy().to_string(),
                ..Default::default()
            };
            let started = Instant::now();
            let (result, code) = summarize_components(
                vec![component],
                &default_status_args(),
                StatusTimer::with_budget(true, std::time::Duration::from_secs(10)),
                stale_controller(),
            )
            .expect("bounded status result");
            assert_eq!(code, 0);
            assert!(started.elapsed() < std::time::Duration::from_secs(12));
            let StatusResult::Summary(output) = result else {
                panic!("summary output")
            };
            let partial = output.partial.expect("typed degraded result");
            assert!(matches!(
                partial.reason,
                "component_git_probe_degraded" | "total_latency_budget_exhausted"
            ));
            if require_degraded_component {
                assert_eq!(partial.degraded_components, vec![expected_component]);
            }
            assert!(
                partial.replay_commands.iter().any(|command| command
                    == &format!("homeboy status --component {expected_component} --timings")),
                "partial output must include a deterministic replay command"
            );
            assert!(output
                .timings
                .iter()
                .any(|timing| timing.phase == expected_phase));
            let pid: i32 = fs::read_to_string(pid_file)
                .expect("hung child pid")
                .trim()
                .parse()
                .expect("numeric pid");
            assert!(
                !homeboy::core::process::pid_is_running(pid as u32),
                "status must terminate Git descendants"
            );
        });
    }

    fn make_git_repo(name: &str) -> (TempDir, std::path::PathBuf) {
        crate::test_support::shared_git_repo_fixture(name)
    }

    fn make_committed_git_repo(name: &str) -> (TempDir, std::path::PathBuf) {
        crate::test_support::shared_committed_git_repo_fixture(name)
    }

    fn empty_status_output() -> StatusOutput {
        StatusOutput {
            command: "status",
            total: 0,
            uncommitted: Vec::new(),
            needs_release: Vec::new(),
            ready_to_deploy: Vec::new(),
            ready_to_deploy_note: None,
            docs_only: Vec::new(),
            behind_upstream: Vec::new(),
            upstream_drift: Vec::new(),
            unreleased_merges: Vec::new(),
            unreleased_merges_note: None,
            timings: Vec::new(),
            clean: 0,
            partial: None,
            controller: controller_staleness::current(),
        }
    }

    fn stale_controller() -> ControllerStaleness {
        controller_staleness::assess(
            &homeboy::core::build_identity::BuildIdentity {
                version: "0.327.0".to_string(),
                git_commit: Some("ed33954781a9".to_string()),
                git_dirty: None,
                display: "homeboy 0.327.0+ed33954781a9".to_string(),
            },
            Some("0.329.1"),
            Some(1_000),
            1_100,
        )
    }

    /// The controller's own freshness is part of the status contract, not an
    /// optional extra: every other freshness signal in this report is measured
    /// against this binary (#11483).
    #[test]
    fn status_output_always_carries_controller_freshness() {
        let output = StatusOutput {
            controller: stale_controller(),
            ..empty_status_output()
        };
        let json = serde_json::to_value(&output).expect("serialize status output");

        assert_eq!(json["controller"]["status"], "behind_minor");
        assert_eq!(json["controller"]["stale"], true);
        assert_eq!(json["controller"]["escalated"], true);
        assert_eq!(json["controller"]["minor_releases_behind"], 2);
        assert_eq!(json["controller"]["latest_version"], "0.329.1");
        assert_eq!(json["controller"]["remediation"], "homeboy upgrade");
    }

    /// A current or unestablished controller must still emit the field, so a
    /// reader can distinguish "checked, current" from "never checked" instead
    /// of reading silence as health.
    #[test]
    fn status_output_emits_controller_freshness_when_not_stale() {
        let json = serde_json::to_value(empty_status_output()).expect("serialize status output");

        let controller = json.get("controller").expect("controller field is present");
        assert!(controller.get("status").and_then(|v| v.as_str()).is_some());
        assert!(controller.get("detail").and_then(|v| v.as_str()).is_some());
    }

    /// Reporting only: the staleness warning is emitted, never turned into a
    /// non-zero exit or an error.
    #[test]
    fn controller_staleness_logging_is_advisory_only() {
        log_controller_staleness(&stale_controller());
        log_controller_staleness(&empty_status_output().controller);
    }

    #[test]
    fn parser_accepts_status_timings() {
        let cli = Cli::try_parse_from(["homeboy", "status", "--timings"])
            .expect("status --timings parses");

        match cli.command {
            Commands::Status(args) => assert!(args.timings),
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn global_status_from_an_unregistered_cwd_is_local_and_bounded() {
        let _guard = CWD_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        crate::test_support::with_isolated_home(|_| {
            // A configured runner with no session is disconnected. The global
            // follow-up must expose that persisted fact without contacting it.
            runner::create(r#"{"id":"disconnected","kind":"local"}"#, false)
                .expect("register disconnected runner");
            let original_cwd = env::current_dir().expect("current dir");
            let dir = TempDir::new().expect("tempdir");
            env::set_current_dir(dir.path()).expect("set unregistered cwd");

            let started = Instant::now();
            let result = run(parse_status(&["homeboy", "status", "--global"]));
            let elapsed = started.elapsed();
            env::set_current_dir(original_cwd).expect("restore cwd");

            let (result, code) = result.expect("global status succeeds");
            assert_eq!(code, 0);
            assert!(
                elapsed.as_secs() < 2,
                "global status exceeded local budget: {elapsed:?}"
            );
            let StatusResult::Global(output) = result else {
                panic!("expected global status output");
            };
            assert_eq!(output.status, "global");
            assert_eq!(output.inventory.projects, 0);
            assert_eq!(output.inventory.components, 0);
            assert_eq!(output.runners.inspected, output.runners.registered);
            assert!(output.runners.registered >= 1);
            assert!(output.runners.disconnected >= 1);
            assert_eq!(output.activity.active, 0);
            assert!(output.drill_down.contains(&"homeboy daemon status"));
        });
    }

    #[test]
    fn global_status_keeps_large_registered_inventories_count_only() {
        crate::test_support::with_isolated_home(|home| {
            let registrations = home.path().join(".config/homeboy/components");
            fs::create_dir_all(&registrations).expect("component registry");
            for index in 0..200 {
                fs::write(
                    registrations.join(format!("component-{index}.json")),
                    serde_json::json!({ "local_path": home.path() }).to_string(),
                )
                .expect("component registration");
            }

            let started = Instant::now();
            let (result, code) = run(parse_status(&["homeboy", "status", "--global"]))
                .expect("global status succeeds");
            assert!(
                started.elapsed().as_secs() < 2,
                "global status must stay within its local inventory budget"
            );
            assert_eq!(code, 0);
            let StatusResult::Global(output) = result else {
                panic!("expected global status output");
            };
            assert_eq!(output.inventory.components, 200);
            let json = serde_json::to_value(output).expect("serialize global status");
            assert!(
                json.get("components").is_none(),
                "inventory must remain count-only"
            );
            assert!(
                serde_json::to_string(&json).expect("global JSON").len() < 8_000,
                "global response must not grow with inventory contents"
            );
        });
    }

    #[test]
    fn status_timings_are_omitted_unless_present() {
        let output = empty_status_output();
        let json = serde_json::to_value(&output).expect("serialize status output");
        assert!(json.get("timings").is_none());

        let output = StatusOutput {
            timings: vec![StatusTiming {
                phase: "inspect_release_state",
                elapsed_ms: 12,
            }],
            ..empty_status_output()
        };
        let json = serde_json::to_value(&output).expect("serialize status output");

        assert_eq!(
            json.get("timings")
                .and_then(|value| value.as_array())
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(json["timings"][0]["phase"], "inspect_release_state");
        assert_eq!(json["timings"][0]["elapsed_ms"], 12);
    }

    #[test]
    fn exhausted_status_budget_returns_typed_partial_output_with_timings() {
        let components = vec![component::Component {
            id: "slow-component".to_string(),
            ..Default::default()
        }];
        let mut timer = StatusTimer::with_budget(true, std::time::Duration::ZERO);
        timer.begin("inspect_release_state");
        timer.finish("inspect_release_state");

        let observations = StatusObservations {
            upstream_drift: vec![UpstreamDrift {
                component_id: "already-visited".to_string(),
                ahead: Some(1),
                behind: Some(0),
                latest_origin_tag: None,
            }],
            ..Default::default()
        };
        let (result, code) = partial_status(
            components,
            0,
            std::collections::HashSet::new(),
            std::collections::HashMap::new(),
            timer,
            stale_controller(),
            observations,
            "inspect_release_state",
            &default_status_args(),
        )
        .expect("partial status succeeds");

        assert_eq!(code, 0);
        let StatusResult::Summary(output) = result else {
            panic!("expected summary output");
        };
        let partial = output.partial.expect("typed partial result");
        assert_eq!(partial.reason, "total_latency_budget_exhausted");
        assert_eq!(partial.phase, "inspect_release_state");
        assert_eq!(partial.omitted_components, vec!["slow-component"]);
        assert_eq!(
            partial.replay_commands,
            vec!["homeboy status --component slow-component --timings"]
        );
        assert_eq!(output.timings[0].phase, "inspect_release_state");
        assert_eq!(output.upstream_drift[0].component_id, "already-visited");
    }

    #[test]
    fn summary_status_bounds_hung_tag_probe_and_preserves_typed_observations() {
        assert_hung_probe_is_bounded(
            "tag --merged HEAD",
            "inspect_release_state",
            "hung-tag",
            true,
        );
    }

    #[test]
    fn summary_status_bounds_hung_release_log_probe_and_preserves_typed_observations() {
        assert_hung_probe_is_bounded(
            "log --no-merges",
            "inspect_release_state",
            "hung-release",
            true,
        );
    }

    #[test]
    fn ready_to_deploy_note_is_omitted_when_no_components_are_clean() {
        let output = empty_status_output();
        let json = serde_json::to_value(&output).expect("serialize status output");

        // ready_to_deploy is empty -> note must not leak into the JSON contract.
        assert!(json.get("ready_to_deploy").is_none());
        assert!(
            json.get("ready_to_deploy_note").is_none(),
            "note should be omitted when ready_to_deploy is empty"
        );
    }

    #[test]
    fn ready_to_deploy_note_clarifies_git_state_only_when_components_are_clean() {
        let output = StatusOutput {
            total: 1,
            ready_to_deploy: vec!["sample-plugin".to_string()],
            ready_to_deploy_note: Some(READY_TO_DEPLOY_NOTE),
            ..empty_status_output()
        };
        let json = serde_json::to_value(&output).expect("serialize status output");

        let note = json
            .get("ready_to_deploy_note")
            .and_then(|v| v.as_str())
            .expect("note present when ready_to_deploy is non-empty");

        // The note must steer operators away from treating git state as a
        // target-accurate deploy backlog (issue #4588).
        assert!(
            note.contains("git-state-only"),
            "note should flag the list as git-state-only"
        );
        assert!(
            note.contains("homeboy status <project>"),
            "note should point at the target-accurate project dashboard"
        );
    }

    #[test]
    fn deployed_version_status_marks_current_version_with_newer_origin_tag_as_pinned_current() {
        let status = deployed_version_dashboard_status(
            &Some("0.139.18".to_string()),
            &Some("0.139.18".to_string()),
            Some("v0.139.19"),
        );

        assert!(matches!(
            status,
            ProjectComponentDashboardStatus::PinnedCurrent
        ));
    }

    #[test]
    fn deployed_version_status_keeps_exact_origin_tag_current() {
        let status = deployed_version_dashboard_status(
            &Some("0.139.18".to_string()),
            &Some("0.139.18".to_string()),
            Some("v0.139.18"),
        );

        assert!(matches!(status, ProjectComponentDashboardStatus::Current));
    }

    #[test]
    fn deployed_version_status_keeps_newer_remote_current() {
        let status = deployed_version_dashboard_status(
            &Some("0.12.2".to_string()),
            &Some("0.12.15".to_string()),
            Some("v0.12.15"),
        );

        assert!(matches!(status, ProjectComponentDashboardStatus::Current));
    }

    #[test]
    fn deployed_version_status_marks_unknown_versions_unknown() {
        let status = deployed_version_dashboard_status(
            &Some("not-a-version".to_string()),
            &Some("1.0.0".to_string()),
            None,
        );

        assert!(matches!(status, ProjectComponentDashboardStatus::Unknown));
    }

    #[test]
    fn parser_accepts_status_path_only() {
        let cli = Cli::try_parse_from(["homeboy", "status", "--path", "/tmp/example", "--full"])
            .expect("status --path parses");

        match cli.command {
            Commands::Status(args) => {
                assert_eq!(args.target, None);
                assert_eq!(args.scope.path.as_deref(), Some("/tmp/example"));
                assert!(args.full);
            }
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn parser_accepts_status_id_with_path() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "status",
            "wordpress-playground",
            "--path",
            "/tmp/wp-playground",
            "--full",
        ])
        .expect("status <id> --path parses");

        match cli.command {
            Commands::Status(args) => {
                assert_eq!(args.target.as_deref(), Some("wordpress-playground"));
                assert_eq!(args.scope.path.as_deref(), Some("/tmp/wp-playground"));
                assert!(args.full);
            }
            _ => panic!("expected status command"),
        }
    }

    fn parse_status(argv: &[&str]) -> StatusArgs {
        match Cli::try_parse_from(argv)
            .expect("status invocation should parse")
            .command
        {
            Commands::Status(args) => args,
            _ => panic!("expected status command"),
        }
    }

    /// Every filter/positional spelling `homeboy status` accepted before
    /// `ScopeArgs` must still parse the same way.
    #[test]
    fn previously_valid_status_invocations_still_parse() {
        let bare = parse_status(&["homeboy", "status"]);
        assert!(bare.target.is_none());
        assert!(bare.scope.is_unscoped());

        let positional = parse_status(&["homeboy", "status", "wordpress-playground"]);
        assert_eq!(positional.target.as_deref(), Some("wordpress-playground"));
        assert!(positional.scope.is_unscoped());

        let filters = parse_status(&[
            "homeboy",
            "status",
            "--all",
            "--uncommitted",
            "--needs-release",
            "--ready",
            "--docs-only",
            "--outdated",
            "--unreleased",
            "--timings",
            "--refresh",
            "--full",
        ]);
        assert!(filters.all);
        assert!(!filters.global);
        assert!(filters.uncommitted);
        assert!(filters.needs_release);
        assert!(filters.ready);
        assert!(filters.docs_only);
        assert!(filters.outdated);
        assert!(filters.unreleased);
        assert!(filters.timings);
        assert!(filters.refresh);
        assert!(filters.full);

        let short_all = parse_status(&["homeboy", "status", "-a"]);
        assert!(short_all.all);

        let global = parse_status(&["homeboy", "status", "--global"]);
        assert!(global.global);
    }

    #[test]
    fn status_scope_selectors_resolve_to_their_variants() {
        assert_eq!(
            parse_status(&["homeboy", "status", "--project", "growth"])
                .scope
                .selection(),
            Some(Scope::Project("growth".to_string()))
        );
        assert_eq!(
            parse_status(&["homeboy", "status", "--component", "homeboy"])
                .scope
                .selection(),
            Some(Scope::Component("homeboy".to_string()))
        );
        assert_eq!(
            parse_status(&["homeboy", "status", "--fleet", "growth"])
                .scope
                .selection(),
            Some(Scope::Fleet("growth".to_string()))
        );
        assert_eq!(
            parse_status(&["homeboy", "status", "--rig", "studio"])
                .scope
                .selection(),
            Some(Scope::Rig("studio".to_string()))
        );
        assert_eq!(
            parse_status(&["homeboy", "status", "--workspace"])
                .scope
                .selection(),
            Some(Scope::Workspace)
        );
    }

    #[test]
    fn status_scope_selectors_conflict_with_each_other() {
        for argv in [
            vec!["homeboy", "status", "--project", "a", "--component", "b"],
            vec!["homeboy", "status", "--component", "a", "--path", "/tmp/a"],
            vec!["homeboy", "status", "--project", "a", "--workspace"],
            vec!["homeboy", "status", "--fleet", "a", "--rig", "b"],
        ] {
            assert!(
                Cli::try_parse_from(&argv).is_err(),
                "conflicting status scopes should be rejected: {argv:?}"
            );
        }
    }

    #[test]
    fn status_path_only_inspects_one_synthetic_component() {
        crate::test_support::with_isolated_home(|_| {
            let (_dir, repo) = make_git_repo("external-repo");
            let args = status_args(None, repo.to_string_lossy().to_string(), false);

            let (result, code) = run(args).expect("status --path succeeds");

            assert_eq!(code, 0);
            match result {
                StatusResult::Summary(output) => {
                    assert_eq!(output.total, 1);
                    assert!(output.upstream_drift.is_empty());
                }
                _ => panic!("expected summary output"),
            }
        });
    }

    #[test]
    fn default_status_from_unregistered_cwd_returns_truthful_compact_snapshot() {
        let _guard = CWD_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        crate::test_support::with_isolated_home(|_| {
            let original_cwd = env::current_dir().expect("current dir");
            let dir = TempDir::new().expect("tempdir");
            env::set_current_dir(dir.path()).expect("set unregistered cwd");

            let started = Instant::now();
            let result = run(default_status_args());
            let elapsed = started.elapsed();

            env::set_current_dir(original_cwd).expect("restore cwd");
            let (result, code) = result.expect("status succeeds from unregistered cwd");

            assert_eq!(code, 0);
            assert!(
                elapsed.as_secs() < 2,
                "unregistered status should fast-return, elapsed={elapsed:?}"
            );
            match result {
                StatusResult::Compact(output) => {
                    assert_eq!(output.status, "compact");
                    assert_eq!(
                        PathBuf::from(&output.cwd).canonicalize().ok(),
                        dir.path().canonicalize().ok()
                    );
                    assert_eq!(output.context.status, "not_checked");
                    assert_eq!(output.context.command, "homeboy status --full");
                    assert!(output.action.contains("homeboy status --global"));
                    assert!(output.action.contains("homeboy status --all"));
                    assert!(!output.controller.detail.is_empty());
                }
                _ => panic!("expected unregistered context output"),
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn default_status_skips_a_stalled_inventory_file_without_spawning_work() {
        let _guard = CWD_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        crate::test_support::with_isolated_home(|home| {
            use std::ffi::CString;

            let inventory = home.path().join(".config/homeboy/components");
            fs::create_dir_all(&inventory).expect("component inventory");
            let stalled_file = inventory.join("stalled.json");
            let stalled_file_c = CString::new(stalled_file.as_os_str().as_encoded_bytes())
                .expect("fifo path has no NUL");
            assert_eq!(unsafe { libc::mkfifo(stalled_file_c.as_ptr(), 0o600) }, 0);

            let original_cwd = env::current_dir().expect("current dir");
            let cwd = TempDir::new().expect("tempdir");
            env::set_current_dir(cwd.path()).expect("set unregistered cwd");
            let started = Instant::now();
            let (result, code) = run(default_status_args()).expect("compact status succeeds");
            env::set_current_dir(original_cwd).expect("restore cwd");

            assert_eq!(code, 0);
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "default status must not read the stalled inventory FIFO"
            );
            let StatusResult::Compact(output) = result else {
                panic!("default status must remain a compact snapshot")
            };
            assert_eq!(output.context.status, "not_checked");
            assert_eq!(output.context.command, "homeboy status --full");
        });
    }

    #[test]
    fn default_status_defers_registered_worktree_inventory_to_full() {
        let _guard = CWD_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        crate::test_support::with_isolated_home(|home| {
            let primary = home.path().join("fixture");
            fs::create_dir_all(&primary).expect("primary directory");
            run_git(&primary, &["init"]);
            fs::write(primary.join("file.txt"), "seed\n").expect("seed file");
            run_git(&primary, &["add", "."]);
            run_git(&primary, &["commit", "-m", "seed"]);

            let registration_dir = home.path().join(".config/homeboy/components");
            fs::create_dir_all(&registration_dir).expect("registration directory");
            let registration = registration_dir.join("fixture.json");
            fs::write(
                &registration,
                serde_json::json!({ "local_path": primary }).to_string(),
            )
            .expect("component registration");
            let registration_before = fs::read_to_string(&registration).expect("registration");

            let worktree = home.path().join("fixture@task");
            run_git(
                &primary,
                &["worktree", "add", "-b", "task", worktree.to_str().unwrap()],
            );

            let original_cwd = env::current_dir().expect("current directory");
            env::set_current_dir(&worktree).expect("set worktree cwd");
            let result = run(default_status_args());
            env::set_current_dir(original_cwd).expect("restore cwd");

            let (result, code) = result.expect("top-level status resolves worktree");
            assert_eq!(code, 0);
            match result {
                StatusResult::Compact(output) => {
                    assert_eq!(output.context.status, "not_checked");
                }
                StatusResult::UnregisteredContext(_) => {
                    panic!("compact status must not classify the worktree")
                }
                StatusResult::Summary(_) => panic!("default status must not traverse inventory"),
                StatusResult::Full(_) => panic!("expected summary status"),
                StatusResult::Dashboard(_) => panic!("expected summary status"),
                StatusResult::Global(_) => panic!("expected summary status"),
                StatusResult::Isolated(_) => panic!("expected summary status"),
                StatusResult::ProbeFallback(_) => panic!("expected summary status"),
            }
            assert_eq!(
                fs::read_to_string(&registration).expect("registration after status"),
                registration_before,
                "status must not attach the ephemeral worktree path"
            );
        });
    }

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .expect("git command");
        assert!(status.success(), "git {:?} failed", args);
    }

    #[test]
    fn parser_accepts_unreleased_filter() {
        let cli = Cli::try_parse_from(["homeboy", "status", "--unreleased"])
            .expect("status --unreleased parses");

        match cli.command {
            Commands::Status(args) => assert!(args.unreleased),
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn unscoped_component_enrichment_requires_an_explicit_scope() {
        for args in [
            StatusArgs {
                uncommitted: true,
                ..default_status_args()
            },
            StatusArgs {
                refresh: true,
                ..default_status_args()
            },
        ] {
            assert!(run(args).is_err());
        }
    }

    #[test]
    fn full_status_rejects_ignored_component_options() {
        for args in [
            StatusArgs {
                full: true,
                refresh: true,
                ..default_status_args()
            },
            StatusArgs {
                full: true,
                uncommitted: true,
                ..default_status_args()
            },
        ] {
            assert!(run(args).is_err());
        }
    }

    #[test]
    fn component_replays_preserve_filter_and_refresh_flags() {
        let args = StatusArgs {
            refresh: true,
            unreleased: true,
            docs_only: true,
            ..default_status_args()
        };
        let commands = replay_component_commands(
            &std::collections::HashSet::from(["component-a".to_string()]),
            &args,
        );

        assert_eq!(
            commands,
            vec!["homeboy status --component component-a --docs-only --unreleased --refresh --timings"]
        );
    }

    #[test]
    fn isolated_probe_replays_full_all_and_registry_scopes() {
        let full = parse_status(&["homeboy", "status", "--full"]);
        let all = parse_status(&["homeboy", "status", "--all"]);
        let component = parse_status(&["homeboy", "status", "--component", "core"]);

        assert_eq!(status_probe_argv(&full), ["homeboy", "status", "--full"]);
        assert_eq!(status_probe_argv(&all), ["homeboy", "status", "--all"]);
        assert_eq!(
            status_probe_argv(&component),
            ["homeboy", "status", "--component", "core"]
        );
    }

    #[test]
    fn simultaneous_git_and_remote_failures_keep_each_component_phase() {
        let phases = project_degraded_component_phases(
            std::collections::HashMap::from([
                (
                    "git-only".to_string(),
                    std::collections::HashSet::from(["inspect_release_state"]),
                ),
                (
                    "both".to_string(),
                    std::collections::HashSet::from(["inspect_upstream_and_unreleased"]),
                ),
            ]),
            &HashMap::from([
                ("remote-only".to_string(), "unreachable".to_string()),
                ("both".to_string(), "unreachable".to_string()),
            ]),
        );

        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0].component_id, "both");
        assert_eq!(
            phases[0].phases,
            ["fetch_remote_versions", "inspect_upstream_and_unreleased"]
        );
        assert_eq!(phases[1].component_id, "git-only");
        assert_eq!(phases[2].component_id, "remote-only");
        assert_eq!(phases[2].phases, ["fetch_remote_versions"]);
    }

    #[test]
    fn unreleased_merges_note_is_omitted_when_empty() {
        let output = empty_status_output();
        let json = serde_json::to_value(&output).expect("serialize status output");

        assert!(
            json.get("unreleased_merges").is_none(),
            "empty unreleased_merges must not leak into the JSON contract"
        );
        assert!(
            json.get("unreleased_merges_note").is_none(),
            "note should be omitted when unreleased_merges is empty"
        );
    }

    #[test]
    fn unreleased_merges_note_present_when_merges_exist() {
        let output = StatusOutput {
            total: 1,
            unreleased_merges: vec![UnreleasedMerge {
                component_id: "extrachill-artist-platform".to_string(),
                latest_tag: Some("v1.11.0".to_string()),
                commits_since_tag: 3,
            }],
            unreleased_merges_note: Some(UNRELEASED_MERGES_NOTE),
            ..empty_status_output()
        };
        let json = serde_json::to_value(&output).expect("serialize status output");

        let note = json
            .get("unreleased_merges_note")
            .and_then(|v| v.as_str())
            .expect("note present when unreleased_merges is non-empty");

        // The note must steer operators away from reading a merged PR as shipped.
        assert!(
            note.contains("merged but NOT released"),
            "note should flag merged-not-released"
        );
        assert!(
            note.contains("not on prod yet"),
            "note should clarify the code is not live"
        );
    }

    #[test]
    fn default_origin_branch_resolves_origin_head_symbolic_ref() {
        let (_dir, repo) = make_committed_git_repo("with-origin");
        // Build a fake "origin" remote by cloning into a bare repo and wiring it up.
        // Create the origin/main remote-tracking ref directly so the resolver
        // has something to find without network access.
        run_git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        run_git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );

        let resolved = default_origin_branch(&repo.to_string_lossy(), &StatusTimer::new(false));
        assert_eq!(resolved.as_deref(), Some("origin/main"));
    }

    #[test]
    fn default_origin_branch_falls_back_to_conventional_branches() {
        let (_dir, repo) = make_committed_git_repo("fallback-origin");
        // No origin/HEAD symbolic ref; only a conventional remote-tracking ref.
        run_git(&repo, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);

        let resolved = default_origin_branch(&repo.to_string_lossy(), &StatusTimer::new(false));
        assert_eq!(resolved.as_deref(), Some("origin/trunk"));
    }

    #[test]
    fn default_origin_branch_none_without_remote_refs() {
        let (_dir, repo) = make_committed_git_repo("no-origin");

        assert!(default_origin_branch(&repo.to_string_lossy(), &StatusTimer::new(false)).is_none());
    }

    #[test]
    fn status_id_with_path_full_uses_explicit_component_id() {
        crate::test_support::with_isolated_home(|_| {
            let (_dir, repo) = make_git_repo("wp-playground-checkout");
            let args = status_args(
                Some("wordpress-playground".to_string()),
                repo.to_string_lossy().to_string(),
                true,
            );

            let (result, code) = run(args).expect("status <id> --path --full succeeds");

            assert_eq!(code, 0);
            match result {
                StatusResult::Full(report) => {
                    assert_eq!(report.context.cwd, repo.to_string_lossy());
                    assert_eq!(report.components.len(), 1);
                    assert_eq!(report.components[0].id, "wordpress-playground");
                    assert_eq!(
                        std::path::Path::new(&report.context.cwd)
                            .join(&report.components[0].path)
                            .canonicalize()
                            .expect("reported component path"),
                        repo.canonicalize().expect("fixture component path")
                    );
                }
                _ => panic!("expected full output"),
            }
        });
    }

    #[test]
    fn upstream_drift_cache_is_component_scoped_in_shared_repos() {
        let (_dir, repo) = make_git_repo("monorepo");
        let component_dir = repo.join("components/demo");
        fs::create_dir_all(&component_dir).expect("component dir");
        let component = component::Component {
            id: "actual-component".to_string(),
            local_path: component_dir.to_string_lossy().to_string(),
            ..Default::default()
        };

        let timer = StatusTimer::new(false);
        let repo_key = upstream_drift_cache_key(&repo.to_string_lossy(), &timer);
        let component_key = upstream_drift_cache_key(&component_dir.to_string_lossy(), &timer);
        assert_eq!(component_key, repo_key);

        let scoped_cache_key = component_cache_key(&component);
        assert_ne!(scoped_cache_key, repo_key);

        let mut git_cache = StatusGitCache::default();
        git_cache.upstream_drift.insert(
            scoped_cache_key,
            Some(UpstreamDrift {
                component_id: "cached-component".to_string(),
                ahead: Some(2),
                behind: Some(1),
                latest_origin_tag: Some("v1.2.3".to_string()),
            }),
        );

        let drift = git_cache
            .fetch_upstream_drift_for(&component, &StatusTimer::new(false))
            .expect("cached drift");

        assert_eq!(drift.component_id, "actual-component");
        assert_eq!(drift.ahead, Some(2));
        assert_eq!(drift.behind, Some(1));
        assert_eq!(drift.latest_origin_tag.as_deref(), Some("v1.2.3"));
    }
}
