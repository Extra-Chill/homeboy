use super::*;

/// Validate declared tools in the exact environment candidate gates will use.
/// This is deliberately generic: callers declare executables and environment
/// mappings while extensions own language-specific discovery.
pub fn preflight_gate_toolchains(
    cwd: &Path,
    policy: &AgentTaskGateEnvironmentPolicy,
    requirements: &[AgentTaskGateToolchainRequirement],
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
    runtime_tmpdir: Option<&Path>,
    timeout: Duration,
) -> Result<()> {
    homeboy_core::observation::phase_timing::time("admit.gate_toolchains", || {
        if let Some(runner_id) = policy.lab_runner.as_deref() {
            return placement::preflight(
                cwd,
                runner_id,
                policy,
                requirements,
                package_artifacts,
                timeout,
            );
        }
        preflight_gate_toolchains_local(
            cwd,
            policy,
            requirements,
            package_artifacts,
            runtime_tmpdir,
            timeout,
            false,
        )
    })
}

pub(crate) fn preflight_verify_gate_options(cwd: &Path, gates: &VerifyGateOptions) -> Result<()> {
    // A focused Cargo filter that names no test in the checkout can never
    // pass; refuse it here instead of after a full provider run.
    super::cargo_selection_admission::reject_unmatched_focused_cargo_filters(
        cwd,
        gates.verify.iter().chain(&gates.private_verify),
    )?;
    let inferred: Vec<_> = gates
        .verify
        .iter()
        .chain(&gates.private_verify)
        .flat_map(|gate| {
            gate_runtime::executables(gate).into_iter().map(|command| {
                AgentTaskGateToolchainRequirement {
                    command,
                    resolve_only: true,
                    probe_arguments: vec![gate.clone()],
                }
            })
        })
        .collect();
    if inferred.is_empty()
        && gates.required_toolchains().is_empty()
        && gates.gate_target_probes().is_empty()
        && gates.gate_package_artifacts.is_empty()
        && gates.gate_environment.extension_inputs.is_empty()
        && gates.gate_environment.data_inputs.is_empty()
        && gates.gate_environment.requirements == AgentTaskGateRequirements::default()
    {
        return Ok(());
    }
    let mut placements = std::collections::BTreeSet::new();
    if gates.gate_environment.lab_runner.is_some() {
        placements.insert(gates.gate_environment.lab_runner.clone());
    } else {
        for command in gates.verify.iter().chain(&gates.private_verify) {
            placements.insert(placement::declared_runner(command)?);
        }
        if gates.test_execution_plan.is_some() || placements.is_empty() {
            placements.insert(None);
        }
    }
    for runner in placements {
        let mut environment = gates.gate_environment.clone();
        environment.lab_runner = runner;
        environment.hydrate_rust_cache &= gates.hydrate_dependencies;
        environment.hydrate_dependencies &= gates.hydrate_dependencies;
        if !inferred.is_empty() {
            preflight_gate_toolchains(
                cwd,
                &environment,
                &inferred,
                &[],
                None,
                gates.gate_timeout(),
            )?;
        }
        preflight_gate_toolchains(
            cwd,
            &environment,
            &gates.required_toolchains(),
            &gates.gate_package_artifacts,
            None,
            gates.gate_timeout(),
        )?;
        for (gate, probe) in gates.gate_target_probes() {
            preflight_gate_toolchains(
                cwd,
                &environment,
                std::slice::from_ref(&probe),
                &[],
                None,
                gates.gate_timeout(),
            )
            .map_err(|error| gate_target_preflight_error(&gate, &probe, error))?;
        }
    }
    Ok(())
}

/// A declared gate whose target does not resolve in the gate environment is a
/// broken declaration, not a toolchain outage: no candidate can repair it and
/// retrying the same declaration cannot succeed.
pub(super) fn gate_target_preflight_error(
    gate: &str,
    probe: &AgentTaskGateToolchainRequirement,
    cause: Error,
) -> Error {
    // A probe the Lab transport never ran says nothing about the declaration;
    // calling it a broken gate would terminalize the Cook (#18121).
    if placement::is_lab_gate_transport_failure(&cause) {
        return cause;
    }
    let missing = cause
        .message
        .find(MISSING_GATE_EXTENSIONS)
        .map(|start| format!("{}, then retry Cook.", &cause.message[start..]));
    let remediation = missing.as_deref().unwrap_or("Declare a gate that runs from the repository itself (for example its own test script), or give the component a portable homeboy.json that provides the test capability, then retry Cook.");
    let mut error = Error::validation_invalid_argument(
        "gate declaration",
        format!(
            "declared gate `{gate}` cannot run in the gate environment: `{} {}` failed. {remediation}",
            probe.command,
            probe.probe_arguments.join(" "),
        ),
        Some(gate.to_string()),
        None,
    );
    error.retryable = Some(false);
    error.details = json!({
        "failure_classification": "gate_declaration",
        "command": gate,
        "probe": { "command": probe.command, "arguments": probe.probe_arguments },
        "remediation": remediation,
        "cause": { "code": cause.code.as_str(), "message": cause.message, "details": cause.details },
    });
    error
}

pub(super) fn preflight_gate_toolchains_local(
    cwd: &Path,
    policy: &AgentTaskGateEnvironmentPolicy,
    requirements: &[AgentTaskGateToolchainRequirement],
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
    runtime_tmpdir: Option<&Path>,
    timeout: Duration,
    prepare_runner_cache: bool,
) -> Result<()> {
    let (policy, _) = validate_package_artifacts(cwd, policy, package_artifacts)?;
    let mut selected_environment = selected_gate_environment(&policy, runtime_tmpdir)?;
    if prepare_runner_cache && !requirements.is_empty() {
        selected_environment.configure_rust_cache(cwd)?;
    }
    selected_environment.configure_component_inputs(cwd)?;
    // The deadline covers declared probe execution. Controller scheduling before
    // the first child starts cannot make an otherwise successful probe fail.
    let mut started = None;
    for requirement in requirements {
        // #18051: accept older controllers' sentinel for one compatibility
        // window; remove this legacy branch once those controllers converge.
        let legacy_resolve_only = requirement.probe_arguments.first().map(String::as_str)
            == Some(gate_runtime::RESOLVE_ONLY);
        if requirement.resolve_only || legacy_resolve_only {
            let path = selected_environment
                .values
                .get("PATH")
                .cloned()
                .or_else(|| {
                    (policy.mode == AgentTaskGateEnvironmentMode::Inherit)
                        .then(|| std::env::var("PATH").ok())
                        .flatten()
                })
                .unwrap_or_else(|| "/usr/bin:/bin".into());
            let command = &requirement.command;
            let resolved = homeboy_engine_primitives::command_path::resolve_command_on_path(
                command,
                std::ffi::OsStr::new(&path),
            );
            if resolved.is_none() {
                return Err(gate_runtime::unavailable(
                    requirement
                        .probe_arguments
                        .get(usize::from(legacy_resolve_only))
                        .map(String::as_str)
                        .unwrap_or(command),
                    command,
                    policy.lab_runner.as_deref().unwrap_or("local"),
                ));
            }
            continue;
        }
        let elapsed = started.map(|started: std::time::Instant| started.elapsed());
        let remaining = timeout.saturating_sub(elapsed.unwrap_or_default());
        if started.is_some() && remaining.is_zero() {
            return Err(toolchain_preflight_error(
                requirement,
                timeout,
                Duration::ZERO,
                None,
                true,
                "the total gate verification deadline was exhausted before this probe started",
            ));
        }
        let mut process = Command::new(&requirement.command);
        process
            .args(&requirement.probe_arguments)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        selected_environment.apply(&mut process);
        homeboy_core::engine::command::isolate_process_tree(&mut process);
        let mut child = process.spawn().map_err(|error| {
            toolchain_preflight_error(
                requirement,
                timeout,
                remaining,
                None,
                false,
                &format!("could not resolve or initialize the executable: {error}"),
            )
        })?;
        let started = *started.get_or_insert_with(std::time::Instant::now);
        let elapsed = started.elapsed();
        let remaining = timeout.saturating_sub(elapsed);
        let supervised = homeboy_core::engine::command::wait_with_bounded_output_supervised(
            &mut child,
            GATE_TOOLCHAIN_CAPTURE_LIMIT_BYTES,
            remaining,
            remaining,
            || false,
            |_, _| Ok(()),
        )
        .map_err(|error| {
            toolchain_preflight_error(
                requirement,
                timeout,
                remaining,
                None,
                false,
                &format!("could not collect bounded probe output: {error}"),
            )
        })?;
        let timed_out = supervised.termination
            == homeboy_core::engine::command::SupervisedCommandTermination::TimedOut;
        let output = supervised.output;
        if timed_out || !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = if timed_out {
                "the probe exceeded its remaining gate verification deadline".to_string()
            } else {
                format!(
                    "the probe exited {}: {}",
                    output.status.code().unwrap_or(1),
                    probe_failure_text(&stderr, &String::from_utf8_lossy(&output.stdout)),
                )
            };
            let reason = match selected_environment.unprovisioned_extensions.as_slice() {
                [] => reason,
                missing => format!(
                    "{reason}; {MISSING_GATE_EXTENSIONS}(s) {}",
                    missing
                        .iter()
                        .map(|(id, command)| format!("`{id}` (provision with `{command}`)"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            return Err(toolchain_preflight_error(
                requirement,
                timeout,
                remaining,
                Some(&output),
                timed_out,
                &reason,
            ));
        }
    }
    Ok(())
}

/// What a failed probe said. Homeboy writes its error envelope to stdout and
/// leaves stderr empty, so an empty stderr falls back to the envelope's
/// `summary`, then to the stdout tail (Extra-Chill/homeboy#16268).
pub(super) fn probe_failure_text(stderr: &str, stdout: &str) -> String {
    if !stderr.trim().is_empty() {
        return text_tail(stderr, 5);
    }
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .and_then(|envelope| {
            envelope
                .get("summary")
                .or_else(|| envelope.pointer("/diagnostics/message"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| text_tail(stdout, 5))
}

pub(super) fn validate_package_artifacts(
    cwd: &Path,
    policy: &AgentTaskGateEnvironmentPolicy,
    requirements: &[AgentTaskGatePackageArtifactRequirement],
) -> Result<(
    AgentTaskGateEnvironmentPolicy,
    Vec<AgentTaskGatePackageArtifactProvenance>,
)> {
    let mut policy = policy.clone();
    let mut provenance = Vec::with_capacity(requirements.len());
    for requirement in requirements {
        if requirement.id.trim().is_empty()
            || requirement.environment.name.trim().is_empty()
            || requirement.required_paths.is_empty()
            || requirement.remediation.is_null()
        {
            return Err(package_artifact_error(
                requirement,
                "each declaration requires an id, environment mapping, required path, and remediation metadata",
                Vec::new(),
            ));
        }
        let mapping = &requirement.environment;
        if mapping.source.is_some() == mapping.default.is_some() {
            return Err(package_artifact_error(
                requirement,
                "environment mapping requires exactly one of source or default",
                Vec::new(),
            ));
        }
        if (mapping.name == "HOME" && policy.isolate_home)
            || (XDG_ENV_VARS.contains(&mapping.name.as_str()) && policy.isolate_xdg)
        {
            return Err(package_artifact_error(
                requirement,
                "environment mapping conflicts with the declared HOME/XDG isolation policy",
                Vec::new(),
            ));
        }
        let value = match (&mapping.source, &mapping.default) {
            (Some(source), None) => preserved_environment_value(source)?,
            (None, Some(default)) if !default.is_empty() => default.clone(),
            _ => {
                return Err(package_artifact_error(
                    requirement,
                    "environment mapping resolved to an empty value",
                    Vec::new(),
                ));
            }
        };
        policy.variables.insert(mapping.name.clone(), value.clone());
        let mut artifacts = Vec::with_capacity(requirement.required_paths.len());
        let mut missing = Vec::new();
        for artifact in &requirement.required_paths {
            let path = Path::new(&artifact.path);
            if path.is_absolute() || artifact.path.trim().is_empty() || artifact.path.contains("..")
            {
                missing.push(artifact.path.clone());
                continue;
            }
            let path = cwd.join(path);
            if !path.exists() {
                missing.push(artifact.path.clone());
                continue;
            }
            let observed = if path.is_file() {
                let bytes = fs::read(&path).map_err(|error| {
                    Error::internal_io(error.to_string(), Some(path.display().to_string()))
                })?;
                Some(format!("sha256:{:x}", Sha256::digest(bytes)))
            } else {
                None
            };
            if let Some(expected) = &artifact.sha256 {
                let actual = observed.as_ref().ok_or_else(|| {
                    package_artifact_error(
                        requirement,
                        "required artifact digest cannot be verified for a non-file path",
                        vec![artifact.path.clone()],
                    )
                })?;
                if expected != actual {
                    missing.push(artifact.path.clone());
                    continue;
                }
            }
            artifacts.push(AgentTaskGateArtifactPathProvenance {
                path: artifact.path.clone(),
                sha256: observed.or_else(|| artifact.sha256.clone()),
            });
        }
        if !missing.is_empty() {
            return Err(package_artifact_error(
                requirement,
                "required artifact paths are unavailable or do not match their declared digest",
                missing,
            ));
        }
        provenance.push(AgentTaskGatePackageArtifactProvenance {
            id: requirement.id.clone(),
            environment: mapping.name.clone(),
            value,
            source: mapping.source.clone(),
            artifacts,
            remediation: requirement.remediation.clone(),
        });
    }
    Ok((policy, provenance))
}

pub(super) fn package_artifact_error(
    requirement: &AgentTaskGatePackageArtifactRequirement,
    message: &str,
    invalid_paths: Vec<String>,
) -> Error {
    let mut error = Error::validation_invalid_argument(
        "gate_package_artifacts",
        format!(
            "package artifact readiness for `{}` failed: {message}",
            requirement.id
        ),
        Some(requirement.id.clone()),
        None,
    );
    error.retryable = Some(true);
    error.details["package_artifact_readiness"] = json!({
        "id": requirement.id,
        "environment": requirement.environment,
        "invalid_paths": invalid_paths,
        "remediation": requirement.remediation,
    });
    error
}

pub(super) fn toolchain_preflight_error(
    requirement: &AgentTaskGateToolchainRequirement,
    timeout: Duration,
    probe_timeout: Duration,
    output: Option<&homeboy_core::engine::command::BoundedCommandOutput>,
    timed_out: bool,
    reason: &str,
) -> Error {
    let mut error = Error::validation_invalid_argument(
        "gate_toolchains",
        format!(
            "gate toolchain preflight failed for `{}` (deadline {} ms): {reason}",
            requirement.command,
            timeout.as_millis(),
        ),
        Some(requirement.command.clone()),
        Some(vec!["Declare the required toolchain environment with --gate-env-from NAME=SOURCE[/suffix], then retry Cook.".to_string()]),
    );
    error.retryable = Some(true);
    error.details["toolchain_preflight"] = json!({
        "command": requirement.command,
        "arguments": requirement.probe_arguments,
        "timeout_ms": timeout.as_millis(),
        "probe_timeout_ms": probe_timeout.as_millis(),
        "timed_out": timed_out,
        "capture_limit_bytes": GATE_TOOLCHAIN_CAPTURE_LIMIT_BYTES,
        "stdout": output.map(|output| json!({
            "tail": String::from_utf8_lossy(&output.stdout),
            "bytes_seen": output.capture.stdout.bytes_seen,
            "bytes_retained": output.capture.stdout.bytes_retained,
            "truncated": output.capture.stdout.truncated,
        })),
        "stderr": output.map(|output| json!({
            "tail": String::from_utf8_lossy(&output.stderr),
            "bytes_seen": output.capture.stderr.bytes_seen,
            "bytes_retained": output.capture.stderr.bytes_retained,
            "truncated": output.capture.stderr.truncated,
        })),
        "remediation": "Declare the required toolchain environment with --gate-env-from NAME=SOURCE[/suffix], then retry Cook.",
    });
    error
}

pub(super) fn set_isolated_environment_variable(
    report: &mut AgentTaskGateEnvironment,
    values: &mut BTreeMap<String, String>,
    name: &str,
    path: PathBuf,
) {
    let value = path.display().to_string();
    values.insert(name.to_string(), value.clone());
    report.sanitized.push(AgentTaskGateEnvironmentVariable {
        name: name.to_string(),
        value,
    });
}
