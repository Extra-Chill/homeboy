use super::*;
use types::{RunnerDoctorOutput, RunnerDoctorStatus, RunnerRepair};

pub fn apply(
    target: &target::RunnerTarget,
    options: &RunnerDoctorOptions,
    report: &mut RunnerDoctorOutput,
) {
    if options.scope != RunnerDoctorScope::LabOffload {
        report.repairs.push(RunnerRepair {
            id: "repair.scope".to_string(),
            status: RunnerDoctorStatus::Warning,
            message:
                "No repairs were applied because --repair is only active for --scope lab-offload"
                    .to_string(),
            commands: Vec::new(),
        });
        return;
    }

    let target::RunnerTarget::Ssh {
        id,
        runner: runner_config,
        client,
        ..
    } = target
    else {
        report.repairs.push(RunnerRepair {
            id: "repair.runner".to_string(),
            status: RunnerDoctorStatus::Warning,
            message: "No Lab daemon repair is available for local runner targets".to_string(),
            commands: Vec::new(),
        });
        return;
    };

    repair_managed_sources(client, report);

    let daemon_failed = report
        .checks
        .iter()
        .any(|check| check.id == "daemon.exec" && check.status == RunnerDoctorStatus::Error);
    if !daemon_failed {
        report.repairs.push(RunnerRepair {
            id: "repair.daemon".to_string(),
            status: RunnerDoctorStatus::Ok,
            message: "Connected Lab daemon did not require repair".to_string(),
            commands: Vec::new(),
        });
        return;
    }

    let commands = vec![
        format!("homeboy runner disconnect {id}"),
        format!("homeboy runner connect {id}"),
    ];
    let disconnect_error = runner::disconnect(id).err();
    // Connect owns lease-safe dead-daemon adoption. A failed disconnect must not
    // force operators through repeated stop/adopt cycles when its authoritative
    // probe has already established that the recorded owner is gone.
    match runner::connect(id) {
        Ok((_, 0)) => {
            report.checks.retain(|check| check.id != "daemon.exec");
            let workspace_root = runner_config.workspace_root.as_deref().unwrap_or(".");
            report
                .checks
                .extend(probes::connected_daemon_exec_checks(id, workspace_root));
            report.repairs.push(RunnerRepair {
                id: "repair.daemon".to_string(),
                status: RunnerDoctorStatus::Ok,
                message: match disconnect_error {
                    Some(error) => format!(
                        "Recovered the Lab runner daemon after bounded disconnect failed ({}) and reran the daemon exec probe",
                        error.message
                    ),
                    None => "Reconnected the Lab runner daemon and reran the daemon exec probe"
                        .to_string(),
                },
                commands,
            });
        }
        Ok((connect_report, exit_code)) => {
            let failure = connect_report
                .failure_message
                .unwrap_or_else(|| format!("runner connect exited with code {exit_code}"));
            report.repairs.push(RunnerRepair {
                id: "repair.daemon".to_string(),
                status: RunnerDoctorStatus::Error,
                message: match disconnect_error {
                    Some(disconnect_error) => format!(
                        "Could not recover Lab daemon after bounded disconnect failed ({}): {}",
                        disconnect_error.message, failure
                    ),
                    None => format!("Could not reconnect Lab daemon: {failure}"),
                },
                commands,
            });
        }
        Err(err) => {
            report.repairs.push(RunnerRepair {
                id: "repair.daemon".to_string(),
                status: RunnerDoctorStatus::Error,
                message: match disconnect_error {
                    Some(disconnect_error) => format!(
                        "Could not recover Lab daemon after bounded disconnect failed ({}): {}",
                        disconnect_error.message, err.message
                    ),
                    None => format!("Could not reconnect Lab daemon: {}", err.message),
                },
                commands,
            });
        }
    }
}

fn repair_managed_sources(client: &SshClient, report: &mut RunnerDoctorOutput) {
    let contracts = homeboy::agents::agent_tasks::provider::provider_runner_source_contracts();
    let plans = runner::plan_managed_runner_source_syncs(&contracts);
    if plans.is_empty() {
        return;
    }

    let mut failed = false;
    for plan in plans {
        let output = client.execute(&plan.script);
        if !output.success {
            failed = true;
            report.repairs.push(RunnerRepair {
                id: format!("repair.managed_source.{}", plan.id),
                status: RunnerDoctorStatus::Error,
                message: format!(
                    "Could not refresh managed runner source `{}`: {}",
                    plan.label,
                    output.stderr.trim()
                ),
                commands: Vec::new(),
            });
            continue;
        }

        report.repairs.push(RunnerRepair {
            id: format!("repair.managed_source.{}", plan.id),
            status: RunnerDoctorStatus::Ok,
            message: format!("Refreshed managed runner source `{}`", plan.label),
            commands: Vec::new(),
        });
    }

    if failed {
        return;
    }

    report
        .checks
        .retain(|check| !check.id.starts_with("lab.managed_source."));
    report
        .checks
        .extend(probes::managed_runner_source_checks(client, &contracts));
}
