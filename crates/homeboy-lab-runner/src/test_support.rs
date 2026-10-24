//! Shared fixtures for lab-runner tests.

use crate::{
    Runner, RunnerKind, RunnerPolicy, RunnerSession, RunnerSessionRole, RunnerSettings,
    RunnerTunnelMode,
};
use chrono::Utc;

/// Re-execute an environment-mutating test alone. Module-local locks cannot
/// protect unrelated readers of PATH (or source-sync policy) in libtest workers.
/// Call before acquiring any environment lock or creating an isolated HOME.
/// Returns true in the parent, where the caller must return immediately.
pub(crate) fn isolated_process() -> bool {
    const CHILD: &str = "HOMEBOY_LAB_ISOLATED_TEST";
    let name = std::thread::current()
        .name()
        .expect("named libtest thread")
        .to_owned();
    if std::env::var(CHILD).as_deref() == Ok(name.as_str()) {
        return false;
    }
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", &name, "--nocapture"])
        .env(CHILD, &name);
    let output = homeboy_core::test_support::bounded_output(command);
    assert!(
        output.status.success(),
        "isolated test {name} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

/// HOME/XDG and core's root/cache overrides are process-global. Run legacy
/// ambient-root fixtures in their own libtest process; root-injected tests can
/// continue in parallel without observing a temporary home being removed.
pub(crate) fn with_isolated_home(body: impl FnOnce(&tempfile::TempDir)) {
    if !isolated_process() {
        homeboy_core::test_support::with_isolated_home(body);
    }
}

#[test]
fn isolated_home_does_not_repoint_parent_roots() {
    let home = std::env::var_os("HOME");
    let path = std::env::var_os("PATH");
    with_isolated_home(|isolated| {
        assert_eq!(std::env::var_os("HOME").unwrap(), isolated.path());
        // Even a panic or a forgotten restoration in a fault fixture cannot
        // publish these changes back into sibling libtest workers.
        std::env::set_var("PATH", "/fault-injection-only");
    });
    assert_eq!(std::env::var_os("HOME"), home);
    // In the child, the deliberately changed PATH is allowed to remain.
    if std::env::var_os("HOMEBOY_LAB_ISOLATED_TEST").is_none() {
        assert_eq!(std::env::var_os("PATH"), path);
    }
}

pub(crate) fn ssh_runner() -> Runner {
    Runner {
        id: "lab".to_string(),
        kind: RunnerKind::Ssh,
        server_id: Some("srv".to_string()),
        workspace_root: Some("/srv/homeboy".to_string()),
        settings: RunnerSettings {
            daemon: true,
            ..Default::default()
        },
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: RunnerPolicy::default(),
    }
}

pub(crate) fn direct_ssh_session(lease_id: &str) -> RunnerSession {
    RunnerSession {
        runner_id: "homeboy-lab".to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some("homeboy-lab".to_string()),
        controller_id: None,
        broker_url: None,
        remote_daemon_address: Some("127.0.0.1:49152".to_string()),
        local_port: Some(49153),
        local_url: Some("http://127.0.0.1:49153".to_string()),
        tunnel_pid: Some(1234),
        tunnel_process_start_identity: None,
        proxy_forward: None,
        remote_daemon_pid: Some(4242),
        remote_daemon_lease_id: Some(lease_id.to_string()),
        homeboy_version: "test".to_string(),
        homeboy_build_identity: Some("homeboy test+abc123".to_string()),
        connected_at: Utc::now().to_rfc3339(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    }
}
