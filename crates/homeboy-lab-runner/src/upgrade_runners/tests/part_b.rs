use super::*;

#[test]
fn managed_immutable_runner_slots_withhold_recovery_without_a_commit_identity() {
    let commands = managed_immutable_runner_recovery_commands_with_commit("homeboy-lab", None);

    assert!(commands.is_empty());
}

#[test]
fn managed_immutable_runner_uses_reconciled_admission_postcondition() {
    let mut before_reconcile = runner_status("homeboy-lab").expect("runner status");
    before_reconcile.active_job_state = RunnerActiveJobState::Available;
    assert!(!managed_immutable_admission_ready(&before_reconcile));

    let mut reconciled = runner_status("homeboy-lab").expect("runner status");
    reconciled.connected = true;
    reconciled.state = RunnerSessionState::Connected;
    reconciled.active_job_state = RunnerActiveJobState::Available;
    assert!(managed_immutable_admission_ready(&reconciled));
}

#[test]
fn managed_immutable_runner_reconciles_a_rotated_but_draining_daemon() {
    assert!(managed_refresh_can_reconcile(1, true, false));
    assert!(!managed_refresh_can_reconcile(1, false, false));
    assert!(!managed_refresh_can_reconcile(1, true, true));
    assert!(!managed_refresh_can_reconcile(0, true, true));
}

#[test]
fn managed_immutable_runner_converges_to_the_installed_controller_not_the_running_build() {
    // `homeboy upgrade` refreshes runners from the pre-upgrade process; the
    // target is the commit of the controller it just installed.
    let installed = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    assert_ne!(
        Some(installed),
        homeboy_product_identity::build_identity()
            .git_commit
            .as_deref()
    );
    assert_eq!(
        managed_immutable_runner_target_revision(Some(&format!("homeboy 0.401.0+{installed}"))),
        Some(installed.to_string())
    );
    assert_eq!(
        managed_immutable_runner_target_revision(Some(&format!(
            "homeboy 0.401.0+{installed}-dirty"
        ))),
        Some(installed.to_string())
    );
    // An identity without a commit falls back to this build, as before.
    assert_eq!(
        managed_immutable_runner_target_revision(Some("homeboy 0.401.0")),
        homeboy_product_identity::build_identity().git_commit
    );
    assert_eq!(
        managed_immutable_runner_target_revision(None),
        homeboy_product_identity::build_identity().git_commit
    );
}
