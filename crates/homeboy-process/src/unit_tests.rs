use super::*;

#[cfg(target_os = "linux")]
#[test]
fn parse_linux_stat_handles_command_names_with_spaces() {
    let stat = "123 (name with spaces) Z 1 456 456 0 -1 0 0 0";
    let process = parse_linux_stat(stat).expect("process stat");

    assert_eq!(process.state, 'Z');
    assert_eq!(process.parent_pid, 1);
    assert_eq!(process.process_group_id, 456);
}

#[cfg(target_os = "linux")]
#[test]
fn descendant_rows_include_children_outside_the_owner_group() {
    assert_eq!(
        descendant_pids_from_rows(&[(10, 1), (11, 10), (12, 11), (13, 1)], 10),
        vec![11, 12]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn force_stop_environment_ownership_requires_an_exact_assignment() {
    let environment = b"HOME=/tmp\0HOMEBOY_DAEMON_STARTUP_TOKEN=lease-token\0";

    assert!(environment_contains_assignment(
        environment,
        b"HOMEBOY_DAEMON_STARTUP_TOKEN=lease-token"
    ));
    assert!(!environment_contains_assignment(
        environment,
        b"HOMEBOY_DAEMON_STARTUP_TOKEN=lease"
    ));
}

#[cfg(all(unix, not(target_os = "linux")))]
#[test]
fn non_linux_ownership_checks_require_the_exact_startup_token_argument() {
    let command = "homeboy daemon supervise --startup-token lease-token --addr 127.0.0.1:0";

    assert!(command_has_option_value(
        command,
        "--startup-token",
        "lease-token"
    ));
    assert!(!command_has_option_value(
        command,
        "--startup-token",
        "lease"
    ));
    assert!(!command_has_option_value(
        "homeboy daemon supervise --startup-token=lease-token",
        "--startup-token",
        "lease-token"
    ));
}

#[cfg(target_os = "windows")]
#[test]
fn containment_assigns_a_suspended_child_to_a_job_before_execution() {
    assert_eq!(
        WINDOWS_CONTAINMENT_CREATION_FLAGS,
        windows_sys::Win32::System::Threading::CREATE_SUSPENDED
    );

    let mut command = Command::new("cmd");
    command.args(["/C", "exit", "0"]);
    let mut containment = ProcessContainment::prepare(&mut command).expect("containment job");
    let mut child = command.spawn().expect("suspended child");
    containment.attach(&mut child).expect("job assignment");
    assert!(child.wait().expect("child exit").success());
    containment
        .terminate_bounded(Duration::from_secs(1))
        .expect("empty job");
}
