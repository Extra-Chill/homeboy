use super::*;

#[cfg(test)]
pub(crate) fn run_gate_command(
    cwd: &Path,
    index: usize,
    command: &str,
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_policy(
        cwd,
        index,
        command,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
    )
}

#[cfg(test)]
pub(crate) fn run_gate_command_with_policy(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_policy_and_runtime_tmpdir(
        cwd,
        index,
        command,
        visibility,
        reveal_policy,
        None,
    )
}

#[cfg(test)]
pub(crate) fn run_gate_command_with_policy_and_runtime_tmpdir(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: Option<&Path>,
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        cwd,
        index,
        command,
        visibility,
        reveal_policy,
        runtime_tmpdir,
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "gate execution inputs remain explicit across lifecycle and provider boundaries"
)]
pub(crate) fn run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: Option<&Path>,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_supervision(
        cwd,
        index,
        command,
        visibility,
        reveal_policy,
        runtime_tmpdir,
        None,
        gate_environment,
        package_artifacts,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "supervision callbacks and durable gate inputs remain independently auditable"
)]
pub fn run_gate_command_with_supervision(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: Option<&Path>,
    supervision: Option<&GateSupervision>,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
) -> Result<AgentTaskGateReport> {
    run_gate_argv_with_supervision(
        cwd,
        index,
        legacy_gate_argv(command),
        command,
        visibility,
        reveal_policy,
        runtime_tmpdir,
        supervision,
        gate_environment,
        package_artifacts,
        None,
    )
    .map(|mut report| {
        report.invocation = Some(AgentTaskGateInvocation::LegacyShell {
            command: command.to_string(),
            shell: LegacyGateShell::NonInteractive,
        });
        report
    })
}

/// Execute a declared test adapter as argv. This is deliberately separate from
/// the legacy shell-gate wrapper above: persisted declared commands never pass
/// through a shell or need their outcome reconstructed from English output.
#[expect(
    clippy::too_many_arguments,
    reason = "supervision callbacks and durable gate inputs remain independently auditable"
)]
pub fn run_declared_test_with_supervision(
    cwd: &Path,
    index: usize,
    plan: &homeboy_engine_primitives::test_execution::TestExecutionPlan,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: Option<&Path>,
    supervision: Option<&GateSupervision>,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
) -> Result<AgentTaskGateReport> {
    let argv = plan
        .declared_command()
        .map_err(|message| Error::invalid_argument("test_execution_plan", message))?
        .to_vec();
    let label = homeboy_engine_primitives::shell::quote_args(&argv);
    run_gate_argv_with_supervision(
        cwd,
        index,
        argv,
        &label,
        visibility,
        reveal_policy,
        runtime_tmpdir,
        supervision,
        gate_environment,
        package_artifacts,
        Some(plan),
    )
    .map(|mut report| {
        report.invocation = Some(AgentTaskGateInvocation::DeclaredTest { plan: plan.clone() });
        report
    })
}

pub(super) fn gate_phase_name(label: Option<&str>, command: &str, index: usize) -> String {
    let source = label.unwrap_or_else(|| {
        command
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("set "))
            .unwrap_or_default()
    });
    let mut slug = String::new();
    for ch in source.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() == 40 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        format!("gate.gate-{index}")
    } else {
        format!("gate.{slug}")
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "shell and declared gates share one process supervisor"
)]
pub(super) fn run_gate_argv_with_supervision(
    cwd: &Path,
    index: usize,
    command_vec: Vec<String>,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: Option<&Path>,
    supervision: Option<&GateSupervision>,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
    declared_plan: Option<&homeboy_engine_primitives::test_execution::TestExecutionPlan>,
) -> Result<AgentTaskGateReport> {
    let run = || {
        homeboy_core::observation::phase_timing::time_with_outcome(
            &gate_phase_name(None, command, index),
            || {
                run_gate_argv(
                    cwd,
                    index,
                    command_vec.clone(),
                    command,
                    visibility,
                    reveal_policy,
                    GateExecution {
                        runtime_tmpdir,
                        timeout: supervision.map(|supervision| supervision.timeout),
                        supervision,
                        baseline_timeout_diagnostic: false,
                        deadline: None,
                    },
                    gate_environment,
                    package_artifacts,
                    declared_plan,
                )
                .inspect(|report| {
                    telemetry::record(&report.environment);
                })
            },
            |report| {
                matches!(
                    report.status,
                    AgentTaskGateStatus::Failed
                        | AgentTaskGateStatus::Unavailable
                        | AgentTaskGateStatus::Deferred
                )
            },
        )
    };
    // Call sites that thread no explicit supervision still run inside the
    // declared scope a Lab request reads.
    let scope = crate::agent_task_promotion::active_gate_supervision();
    let cancelled = || {
        supervision
            .or(scope.as_deref())
            .is_some_and(|supervision| (supervision.is_cancelled)())
    };
    regate_after_supervision_stop(run, cancelled)
}

/// Run a gate, re-gating a supervision stop once and re-running a candidate
/// failure once.
///
/// A supervision stop (wall-clock or no-progress limit) never reached the
/// command's verdict, so it is re-run once on the same candidate, under the
/// same declared budgets, without spending a provider attempt. A stop that
/// repeats is classified as unavailable verification, never a candidate
/// failure (#17958, #18554).
fn regate_after_supervision_stop(
    run: impl Fn() -> Result<AgentTaskGateReport>,
    cancelled: impl Fn() -> bool,
) -> Result<AgentTaskGateReport> {
    let mut report = run()?;
    let mut supervision_regate = None;
    if report.stopped_by_supervision() && !cancelled() {
        supervision_regate = report.failure_evidence.take();
        report = run()?;
    }
    if report.stopped_by_supervision() {
        report.classify_supervision_stop_as_unavailable();
        report.supervision_regate = supervision_regate;
        return Ok(report);
    }
    report.supervision_regate = supervision_regate;
    if !report.reruns_on_failure() {
        return Ok(report);
    }
    // A command that ran and failed is re-run once in the same workspace: a
    // single failure under host load (a test timeout, a race) is not proof
    // against the candidate. Only a second failure refuses it.
    let mut rerun = run()?;
    rerun.supervision_regate = report.supervision_regate.take();
    if rerun.status != AgentTaskGateStatus::Succeeded {
        rerun.classify_supervision_stop_as_unavailable();
        return Ok(rerun);
    }
    Ok(AgentTaskGateReport {
        flaky: report.failure_evidence,
        ..rerun
    })
}

/// Comparison base carried by generated component lint gates, also used by
/// canonical Lab materialization to retain the commit object.
/// Look up a verdict without dependency, toolchain, or workspace setup.
/// Opaque package and extension inputs conservatively execute instead.
pub(crate) fn cached_gate(
    cwd: &Path,
    index: usize,
    argv: &[String],
    plan: Option<&homeboy_engine_primitives::test_execution::TestExecutionPlan>,
    gates: &VerifyGateOptions,
    visibility: AgentTaskGateVisibility,
    reveal: AgentTaskGateRevealPolicy,
) -> Option<AgentTaskGateReport> {
    if !gates.gate_package_artifacts.is_empty() || gates.gate_environment.lab_runner.is_some() {
        return None;
    }
    let mut policy = gates.gate_environment.clone();
    policy.hydrate_rust_cache &= gates.hydrate_dependencies;
    policy.hydrate_dependencies &= gates.hydrate_dependencies;
    let scratch = tempfile::tempdir().ok()?;
    let selected = selected_gate_environment(&policy, Some(scratch.path())).ok()?;
    let mut process = Command::new(argv.first()?);
    process.args(&argv[1..]);
    selected.apply(&mut process);
    let timeout = homeboy_engine_primitives::test_execution::TestExecutionPlan::with_suite_timeout(
        gates.gate_timeout(),
    )
    .ok()?;
    let (name, value) = plan.unwrap_or(&timeout).suite_timeout_env();
    process.env(name, value);
    verdict::GateVerdict::prepare(
        cwd,
        &process,
        &policy,
        &selected.report,
        Some(scratch.path()),
        plan,
    )?
    .passed(index, visibility, reveal)
}

pub fn component_lint_base(command: &str) -> Option<&str> {
    command
        .split("HOMEBOY_LINT_BASE=")
        .nth(1)?
        .split_whitespace()
        .next()
}

#[expect(
    clippy::too_many_arguments,
    reason = "shell and declared gates share one process executor"
)]
pub(super) fn run_gate_argv(
    cwd: &Path,
    index: usize,
    command_vec: Vec<String>,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    execution: GateExecution<'_>,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
    declared_plan: Option<&homeboy_engine_primitives::test_execution::TestExecutionPlan>,
) -> Result<AgentTaskGateReport> {
    if let Some(base) = component_lint_base(command) {
        if !Command::new("git")
            .args(["cat-file", "-e", &format!("{base}^{{commit}}")])
            .current_dir(cwd)
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return Ok(placement::gate_non_execution_report(
                index,
                command_vec,
                visibility,
                reveal_policy,
                &Error::invalid_argument(
                    "gate.base",
                    format!("recorded Cook base {base} is unavailable"),
                ),
            ));
        }
    }
    let runner_id = gate_environment
        .lab_runner
        .clone()
        .map(Ok)
        .unwrap_or_else(|| {
            placement::declared_runner(command).map(|runner| runner.unwrap_or_default())
        })?;
    if !runner_id.is_empty() {
        // Call sites that do not thread explicit supervision (for example the
        // verified-base differential replays) still run inside the declared
        // `with_gate_supervision` scope. Fall back to that scope before any
        // built-in default so the request the Lab runner enforces carries the
        // operator's declared budgets instead of silently replacing them
        // with 300s/1800s (#15856).
        let scope = crate::agent_task_promotion::active_gate_supervision();
        let supervision = execution.supervision.or(scope.as_deref());
        let candidate =
            crate::agent_task_promotion::candidate_fingerprint(&cwd.display().to_string())?;
        let timeout_seconds = execution
            .timeout
            .or_else(|| supervision.map(|policy| policy.timeout))
            .unwrap_or(Duration::from_secs(1800))
            .as_secs();
        let request = placement::LabGateRequest {
            schema: placement::LabGateRequestSchema::V2,
            preflight_only: false,
            toolchains: vec![],
            runner_id,
            candidate_subpath: placement::candidate_subpath(cwd, &candidate)?,
            candidate,
            index,
            argv: command_vec,
            label: command.to_string(),
            visibility,
            reveal_policy,
            environment: with_controller_extension_sources(gate_environment, cwd),
            package_artifacts: package_artifacts.to_vec(),
            declared_plan: declared_plan.cloned(),
            timeout_seconds,
            // A disabled (zero) no-progress window travels as the wall-clock
            // limit, which every runner version reads as "bounded by the gate
            // timeout only".
            no_progress_timeout_seconds: match supervision
                .map(|policy| policy.no_progress_timeout.as_secs())
                .unwrap_or(300)
            {
                0 => timeout_seconds.max(1),
                seconds => seconds,
            },
        };
        return placement::dispatch(cwd, &request, supervision);
    }
    run_gate_argv_local(
        cwd,
        index,
        command_vec,
        command,
        visibility,
        reveal_policy,
        execution,
        gate_environment,
        package_artifacts,
        declared_plan,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "admitted runner gates reuse the local process owner without recursively routing"
)]
pub(super) fn run_gate_argv_local(
    cwd: &Path,
    index: usize,
    command_vec: Vec<String>,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    execution: GateExecution<'_>,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
    declared_plan: Option<&homeboy_engine_primitives::test_execution::TestExecutionPlan>,
) -> Result<AgentTaskGateReport> {
    if let Some(base) = component_lint_base(command) {
        let resolved = Command::new("git")
            .args(["cat-file", "-e", &format!("{base}^{{commit}}")])
            .current_dir(cwd)
            .output();
        if !resolved.is_ok_and(|output| output.status.success()) {
            let error = Error::invalid_argument(
                "gate.base",
                format!("recorded Cook base {base} is unavailable"),
            );
            return Ok(placement::gate_non_execution_report(
                index,
                command_vec,
                visibility,
                reveal_policy,
                &error,
            ));
        }
    }
    let mut process = Command::new(&command_vec[0]);
    process
        .args(&command_vec[1..])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (gate_environment, package_artifacts) =
        validate_package_artifacts(cwd, gate_environment, package_artifacts)?;
    let mut selected_environment =
        selected_gate_environment(&gate_environment, execution.runtime_tmpdir)?;
    selected_environment.report.package_artifacts = package_artifacts;
    selected_environment.apply(&mut process);
    if let Some(plan) = declared_plan {
        let (name, value) = plan.suite_timeout_env();
        process.env(name, value);
    }
    // Legacy shell gates inherit Cook's deadline. Declared plans already set
    // their own deadline above, so supervision cannot silently rewrite it.
    if let Some(supervision) = execution.supervision.filter(|_| declared_plan.is_none()) {
        let plan =
            homeboy_engine_primitives::test_execution::TestExecutionPlan::with_suite_timeout(
                supervision.timeout,
            )
            .expect("Cook gate timeout is normalized to a positive duration");
        let (name, value) = plan.suite_timeout_env();
        process.env(name, value);
    }
    if execution.supervision.is_some()
        && !homeboy_core::engine::command::supports_process_tree_isolation()
    {
        return Err(Error::validation_invalid_argument(
            "gate_supervision",
            "durable gate cancellation requires Unix process-group isolation on this host",
            None,
            None,
        ));
    }
    let verdict = verdict::GateVerdict::prepare(
        cwd,
        &process,
        &gate_environment,
        &selected_environment.report,
        execution.runtime_tmpdir,
        declared_plan,
    );
    if let Some(report) = verdict
        .as_ref()
        .and_then(|verdict| verdict.passed(index, visibility, reveal_policy))
    {
        return Ok(report);
    }
    selected_environment.report.phase_name = Some(gate_phase_name(None, command, index));
    selected_environment.report.host_pressure_start = Some(telemetry::host_pressure());
    let verdict_started = Instant::now();
    selected_environment.configure_rust_cache(cwd)?;
    selected_environment.configure_cargo_target(cwd, gate_environment.shared_cargo_target)?;
    selected_environment.configure_component_inputs(cwd)?;
    // Do not clear again: the declared suite deadline is already applied.
    process.envs(&selected_environment.values);
    // Every deterministic gate is contained the same way regardless of
    // whether the caller supervises it: the gate spawns build/test tooling
    // (`cargo test -p ... --lib` and friends) that outlives a plain
    // `Command::spawn`. Without a process group of its own and a controller-
    // death guard, killing the agent process that is running this gate
    // (operator `kill -9`, OOM, supervisor teardown) leaves that tooling
    // reparented to init and holding the shared cargo build/artifact lock
    // indefinitely (#13649).
    let mut containment = AgentTaskProcessContainment::prepare(&mut process).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("contain deterministic gate {command}")),
        )
    })?;
    execution_start::before_command_start()?;
    let resources_before = telemetry::children();
    let child = process.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run deterministic gate {command}")),
        )
    })?;
    if let Err(error) = containment.attach(child) {
        return Err(Error::internal_io(
            error.to_string(),
            Some(format!("guard deterministic gate {command}")),
        ));
    }
    let process_group = containment.leader_pid().expect("attached gate owner");
    let execution_identity = containment
        .owner_mut()
        .expect("attached gate owner")
        .identity();
    let timeout = declared_plan
        .map(|plan| plan.suite_timeout())
        .or(execution.timeout);
    let timeout = match execution.deadline {
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            Some(timeout.map_or(remaining, |timeout| timeout.min(remaining)))
        }
        None => timeout,
    };
    let (mut output, termination, progress_instrumented) = if let Some(timeout) = timeout {
        let supervision = execution.supervision;
        if let Some(supervision) = supervision {
            if let Err(error) = (supervision.on_spawn)(process_group, command) {
                if let Err(cleanup_error) = containment.terminate_live() {
                    return Err(Error::internal_io(
                        format!(
                            "durable gate registration failed ({error}); failed to terminate and reap its child: {cleanup_error}"
                        ),
                        Some(format!("supervise deterministic gate {command}")),
                    ));
                }
                return Err(error);
            }
        }
        let supervised =
            homeboy_core::engine::command::wait_with_bounded_output_supervised_with_progress_owned(
                containment.owner_mut().expect("attached gate owner"),
                65_536,
                timeout,
                supervision
                    .map(|supervision| supervision.no_progress_timeout)
                    .filter(|limit| !limit.is_zero()),
                supervision
                    .map(|supervision| supervision.heartbeat_interval)
                    .unwrap_or(Duration::from_secs(5)),
                None,
                || supervision.is_some_and(|supervision| (supervision.is_cancelled)()),
                |heartbeat| {
                    let Some(supervision) = supervision else {
                        return Ok(());
                    };
                    // Durable status must never become a private-gate output channel.
                    let live_status = AgentTaskGateLiveStatus {
                        visibility,
                        reveal_policy,
                        elapsed_ms: heartbeat.elapsed.as_millis(),
                        last_progress_ms_ago: heartbeat
                            .last_progress_elapsed
                            .map(|elapsed| elapsed.as_millis()),
                        progress: heartbeat.progress,
                        output_tail: if visibility == AgentTaskGateVisibility::Private {
                            "private gate output withheld".to_string()
                        } else {
                            heartbeat.output_tail
                        },
                    };
                    (supervision.on_heartbeat)(&live_status).map_err(|error| {
                        std::io::Error::other(format!(
                            "persist deterministic gate heartbeat: {error}"
                        ))
                    })
                },
            )
            .map_err(|error| {
                Error::internal_io(
                    format!(
                        "{error}; execution owner wait_pid={}, root_pid={}, root_pgid={:?}",
                        execution_identity.wait_pid,
                        execution_identity.root_pid,
                        execution_identity.root_pgid,
                    ),
                    Some(format!("supervise deterministic gate {command}")),
                )
            })?;
        (
            supervised.output,
            supervised.termination,
            supervised.progress_instrumented,
        )
    } else {
        let output = homeboy_core::engine::command::wait_with_bounded_output_until_cancelled_owned(
            containment.owner_mut().expect("attached gate owner"),
            65_536,
            || false,
        )
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("run deterministic gate {command}")),
            )
        })?;
        (
            output,
            homeboy_core::engine::command::SupervisedCommandTermination::Completed,
            false,
        )
    };
    // The wait loops above already reap survivors once they observe the
    // leader exit; this is the belt-and-suspenders reap for every remaining
    // return path (including the ones below that can still error out before
    // the report is built).
    let _ = containment.reap_after_exit();
    if execution.baseline_timeout_diagnostic
        && termination == homeboy_core::engine::command::SupervisedCommandTermination::TimedOut
    {
        output.stderr.extend_from_slice(
            format!(
                "\nbaseline gate exceeded {} ms and was cancelled",
                timeout.expect("bounded baseline gate").as_millis()
            )
            .as_bytes(),
        );
    }
    let termination = match termination {
        homeboy_core::engine::command::SupervisedCommandTermination::Completed => {
            AgentTaskGateTermination::Completed
        }
        homeboy_core::engine::command::SupervisedCommandTermination::Cancelled => {
            AgentTaskGateTermination::Cancelled
        }
        homeboy_core::engine::command::SupervisedCommandTermination::TimedOut => {
            AgentTaskGateTermination::TimedOut
        }
        homeboy_core::engine::command::SupervisedCommandTermination::NoProgress => {
            AgentTaskGateTermination::NoProgress
        }
    };
    let runner_exit_code = match termination {
        AgentTaskGateTermination::TimedOut => 124,
        AgentTaskGateTermination::NoProgress => 125,
        _ => output.status.code().unwrap_or(1),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    // Declared adapters own their typed result. Legacy shell gates retain the
    // historical Cargo-output projection for compatibility.
    let test_result = declared_plan
        .is_none()
        .then(|| cargo_test_result(command, runner_exit_code, &stdout, &stderr))
        .flatten();
    let cargo_selection = declared_plan
        .is_none()
        .then(|| {
            cargo_selection(
                command,
                &command_vec,
                &stdout,
                &stderr,
                test_result.as_ref(),
            )
        })
        .flatten();
    let exit_code = effective_gate_exit_code(runner_exit_code, cargo_selection.as_ref());
    let failure_evidence = (exit_code != 0).then(|| {
        gate_budget_failure_evidence(
            command,
            exit_code,
            &stdout,
            &stderr,
            termination,
            progress_instrumented,
            if termination == AgentTaskGateTermination::NoProgress {
                execution
                    .supervision
                    .map(|supervision| supervision.no_progress_timeout)
            } else {
                timeout
            },
        )
        .unwrap_or_else(|| {
            gate_failure_evidence(
                command,
                exit_code,
                &stdout,
                &stderr,
                cargo_selection.as_ref(),
            )
        })
    });

    selected_environment.report.resources =
        telemetry::delta(resources_before, telemetry::children());
    selected_environment.report.host_pressure_end = Some(telemetry::host_pressure());
    homeboy_core::observation::phase_timing::time("publish.gate_cargo_target", || {
        selected_environment.finish_cargo_target()
    })?;
    let mut report = AgentTaskGateReport::new(
        format!("gate-{index}"),
        command_vec,
        exit_code,
        stdout,
        stderr,
        failure_evidence,
        visibility,
        reveal_policy,
        selected_environment.report,
    );
    report.test_result = test_result;
    report.cargo_selection = cargo_selection;
    report.termination = termination;
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        report.signal = output.status.signal();
    }
    report.test_execution_outcome = declared_plan.map(|_| match termination {
        AgentTaskGateTermination::Completed if exit_code == 0 => {
            homeboy_engine_primitives::test_execution::TestExecutionOutcome::Passed
        }
        AgentTaskGateTermination::Completed => {
            homeboy_engine_primitives::test_execution::TestExecutionOutcome::Failed
        }
        AgentTaskGateTermination::TimedOut | AgentTaskGateTermination::NoProgress => {
            homeboy_engine_primitives::test_execution::TestExecutionOutcome::TimedOut
        }
        AgentTaskGateTermination::Cancelled => {
            homeboy_engine_primitives::test_execution::TestExecutionOutcome::Cancelled
        }
    });
    report.capture = AgentTaskGateCapture {
        stdout: output.capture.stdout,
        stderr: output.capture.stderr,
    };
    if let Some(verdict) = verdict {
        verdict.record(cwd, &mut report, verdict_started.elapsed());
    }
    Ok(report)
}

/// Run a comparison gate with a hard wall-clock limit. The bounded path keeps
/// candidate adoption inspectable instead of allowing a known-red baseline to
/// consume an unbounded second broad-suite run.
#[expect(
    clippy::too_many_arguments,
    reason = "legacy gate provider entrypoint preserves mixed-version callers"
)]
pub(crate) fn run_gate_command_with_timeout(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: &Path,
    timeout: Duration,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
) -> Result<AgentTaskGateReport> {
    run_gate_argv(
        cwd,
        index,
        legacy_gate_argv(command),
        command,
        visibility,
        reveal_policy,
        GateExecution {
            runtime_tmpdir: Some(runtime_tmpdir),
            timeout: Some(timeout),
            supervision: None,
            baseline_timeout_diagnostic: true,
            deadline: None,
        },
        gate_environment,
        package_artifacts,
        None,
    )
    .map(|mut report| {
        report.invocation = Some(AgentTaskGateInvocation::LegacyShell {
            command: command.to_string(),
            shell: LegacyGateShell::NonInteractive,
        });
        report
    })
}

pub(crate) fn run_declared_test_with_timeout(
    cwd: &Path,
    index: usize,
    plan: &homeboy_engine_primitives::test_execution::TestExecutionPlan,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: &Path,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
) -> Result<AgentTaskGateReport> {
    let argv = plan
        .declared_command()
        .map_err(|message| Error::invalid_argument("test_execution_plan", message))?
        .to_vec();
    let label = homeboy_engine_primitives::shell::quote_args(&argv);
    run_gate_argv(
        cwd,
        index,
        argv,
        &label,
        visibility,
        reveal_policy,
        GateExecution {
            runtime_tmpdir: Some(runtime_tmpdir),
            timeout: Some(plan.suite_timeout()),
            supervision: None,
            baseline_timeout_diagnostic: true,
            deadline: None,
        },
        gate_environment,
        package_artifacts,
        Some(plan),
    )
    .map(|mut report| {
        report.invocation = Some(AgentTaskGateInvocation::DeclaredTest { plan: plan.clone() });
        report
    })
}
