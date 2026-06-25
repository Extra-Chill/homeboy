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
    let disconnected = runner::disconnect(id);
    if let Err(err) = disconnected {
        report.repairs.push(RunnerRepair {
            id: "repair.daemon".to_string(),
            status: RunnerDoctorStatus::Error,
            message: format!("Could not disconnect stale Lab daemon: {}", err.message),
            commands,
        });
        return;
    }

    match runner::connect(id) {
        Ok(_) => {
            report.checks.retain(|check| check.id != "daemon.exec");
            let workspace_root = runner_config.workspace_root.as_deref().unwrap_or(".");
            report
                .checks
                .extend(probes::connected_daemon_exec_checks(id, workspace_root));
            report.repairs.push(RunnerRepair {
                id: "repair.daemon".to_string(),
                status: RunnerDoctorStatus::Ok,
                message: "Reconnected the Lab runner daemon and reran the daemon exec probe"
                    .to_string(),
                commands,
            });
        }
        Err(err) => {
            report.repairs.push(RunnerRepair {
                id: "repair.daemon".to_string(),
                status: RunnerDoctorStatus::Error,
                message: format!("Could not reconnect Lab daemon: {}", err.message),
                commands,
            });
        }
    }
}

fn repair_managed_sources(client: &SshClient, report: &mut RunnerDoctorOutput) {
    let contracts = homeboy::core::agent_tasks::provider::provider_runner_source_contracts();
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
