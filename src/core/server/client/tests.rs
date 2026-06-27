use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::super::{
    ManagedSshSession, ManagedSshSessionOutput, Server, ServerAuthMode, ServerSessionConfig,
};
use super::delegated::{DELEGATED_RUN_POLL_MS_ENV, DELEGATED_RUN_STATUS_FILE_ENV};
use super::host::{get_local_ips, is_local_host};
use super::local_exec::{
    execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, execute_local_command_stderr_passthrough,
};
use super::ssh_client::{
    build_secret_env_stdin_block, wrap_command_with_secret_env_read_loop,
    SECRET_ENV_STDIN_SENTINEL,
};
use super::{CommandOutput, SshClient};

#[test]
fn secret_env_values_stream_over_stdin_not_command_argv() {
    // OAuth/API tokens that earlier went inline as `export KEY=value` in the
    // SSH command argv (visible in the controller `ps` table and the remote
    // login-shell argv) must now travel over stdin instead.
    let secret_env = std::collections::BTreeMap::from([
        (
            "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
            "super-secret-access-token".to_string(),
        ),
        (
            "OPENAI_API_KEY".to_string(),
            "sk-do-not-leak-this".to_string(),
        ),
    ]);
    let command_line = "cd /srv/app && node run-headless-loop.cjs";

    let wrapped = wrap_command_with_secret_env_read_loop(command_line);
    let block = String::from_utf8(build_secret_env_stdin_block(&secret_env)).expect("utf8 block");

    // The command argv (what lands in `ps`) must carry neither the secret
    // values nor even the secret env names.
    assert!(
        !wrapped.contains("super-secret-access-token"),
        "argv must not contain the access token: {wrapped}"
    );
    assert!(
        !wrapped.contains("sk-do-not-leak-this"),
        "argv must not contain the api key: {wrapped}"
    );
    assert!(
        !wrapped.contains("AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN"),
        "argv must not contain the secret env name: {wrapped}"
    );
    assert!(
        wrapped.contains(command_line),
        "argv must still run the original command: {wrapped}"
    );

    // The stdin block carries the assignments and the terminating sentinel.
    assert!(block.contains("AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN=super-secret-access-token"));
    assert!(block.contains("OPENAI_API_KEY=sk-do-not-leak-this"));
    assert!(block.trim_end().ends_with(SECRET_ENV_STDIN_SENTINEL));
}

#[test]
fn empty_secret_env_block_carries_only_the_sentinel() {
    let block =
        String::from_utf8(build_secret_env_stdin_block(&std::collections::BTreeMap::new()))
            .expect("utf8 block");
    assert_eq!(block, format!("{SECRET_ENV_STDIN_SENTINEL}\n"));
}

#[test]
fn test_non_local_hosts() {
    assert!(!is_local_host("example.com"));
    assert!(!is_local_host("192.168.1.1")); // private but not this machine (unless it is)
    assert!(!is_local_host("8.8.8.8"));
}

#[test]
fn test_own_ip_detected_as_local() {
    // Get this machine's IPs and verify they're detected as local
    if let Some(ips) = get_local_ips() {
        for ip in &ips {
            let ip_str = ip.to_string();
            assert!(
                is_local_host(&ip_str),
                "Machine's own IP {} should be detected as local",
                ip_str
            );
        }
    }
}

#[test]
fn test_execute_local_command_in_dir() {
    let output = execute_local_command_in_dir("sleep 0.2", None, None);

    assert!(output.success);
    let child = output.child_resource.expect("child resource summary");
    assert!(child.child.root_pid > 0);
    assert_eq!(child.child.command_label, "sleep 0.2");
    assert!(child.duration_ms > 0);
    assert!(
        child.peak.sampled_peak_rss_bytes.is_some() || !child.warnings.is_empty(),
        "resource probes should either sample RSS or explain why they could not"
    );
    if child.peak.sampled_peak_rss_bytes.is_some() {
        assert!(!child.samples.is_empty());
        assert!(child.sampled_peak_at_ms.is_some());
        assert!(child.sampled_peak_child_count.is_some());
    }
}

#[test]
fn delegated_terminal_failure_stops_passthrough_wrapper() {
    let dir = tempfile::tempdir().expect("temp dir");
    let status_file = dir.path().join("delegated-run-status.json");
    let status_file_for_writer = status_file.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        std::fs::write(
            status_file_for_writer,
            r#"{"status":"failed","message":"provider runtime failed before ready"}"#,
        )
        .expect("write delegated status");
    });

    let status_path = status_file.to_string_lossy().to_string();
    let poll_ms = "25";
    let env = [
        (DELEGATED_RUN_STATUS_FILE_ENV, status_path.as_str()),
        (DELEGATED_RUN_POLL_MS_ENV, poll_ms),
    ];
    let started = Instant::now();
    let output =
        execute_local_command_passthrough("while true; do sleep 1; done", None, Some(&env));
    writer.join().expect("writer joins");

    assert!(!output.success);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "delegated terminal failure should stop the wrapper promptly"
    );
    assert!(output
        .stderr
        .contains("Delegated runtime reached terminal failure status `failed`"));
    assert!(output
        .stderr
        .contains("provider runtime failed before ready"));
    assert!(output.child_resource.is_some());
}

#[test]
fn delegated_terminal_failure_stops_captured_wrapper() {
    let dir = tempfile::tempdir().expect("temp dir");
    let status_file = dir.path().join("delegated-run-status.json");
    let status_file_for_writer = status_file.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        std::fs::write(
            status_file_for_writer,
            r#"{"status":"failed","message":"provider runtime failed before ready"}"#,
        )
        .expect("write delegated status");
    });

    let status_path = status_file.to_string_lossy().to_string();
    let poll_ms = "25";
    let env = [
        (DELEGATED_RUN_STATUS_FILE_ENV, status_path.as_str()),
        (DELEGATED_RUN_POLL_MS_ENV, poll_ms),
    ];
    let started = Instant::now();
    let output = execute_local_command_in_dir("while true; do sleep 1; done", None, Some(&env));
    writer.join().expect("writer joins");

    assert!(!output.success);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "delegated terminal failure should stop captured wrapper promptly"
    );
    assert!(output
        .stderr
        .contains("Delegated runtime reached terminal failure status `failed`"));
    assert!(output
        .stderr
        .contains("provider runtime failed before ready"));
    assert!(output.child_resource.is_some());
}

#[test]
fn test_upload_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("source.txt");
    let target = dir.path().join("target with spaces.txt");
    std::fs::write(&source, "uploaded through stdin\n").expect("write source");
    let client = SshClient {
        host: "localhost".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        auth: None,
        is_local: true,
        env: HashMap::new(),
    };

    let output = client.upload_file(&source.to_string_lossy(), &target.to_string_lossy());

    assert!(output.success, "upload failed: {}", output.stderr);
    assert_eq!(
        std::fs::read_to_string(target).expect("read target"),
        "uploaded through stdin\n"
    );
}

#[test]
fn test_download_file_copies_large_local_file_without_stdout_payload() {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("large-source.json");
    let target = dir.path().join("large-target.json");
    let payload = format!("{{\"result\":\"{}\"}}\n", "x".repeat(5 * 1024 * 1024));
    std::fs::write(&source, &payload).expect("write source");
    let client = SshClient {
        host: "localhost".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        auth: None,
        is_local: true,
        env: HashMap::new(),
    };

    let output = client.download_file(&source.to_string_lossy(), &target.to_string_lossy());

    assert!(output.success, "download failed: {}", output.stderr);
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read_to_string(target).expect("read target"),
        payload
    );
}

#[test]
fn managed_session_config_adds_controlmaster_args() {
    let server = Server {
        id: "bastion".to_string(),
        aliases: Vec::new(),
        host: "bastion.example.test".to_string(),
        user: "deploy".to_string(),
        port: 2222,
        identity_file: None,
        kind: Some("password-gated".to_string()),
        auth: Some(super::super::ServerAuth {
            mode: ServerAuthMode::KeyPlusPasswordControlmaster,
            session: ServerSessionConfig {
                control_path: Some("/tmp/homeboy-test-%h-%p-%r".to_string()),
                persist: Some("4h".to_string()),
            },
        }),
        env: HashMap::new(),
        runner: None,
    };

    let client = SshClient::from_server(&server, "bastion").expect("client");
    let args = client.build_ssh_args(Some("uptime"), false);

    assert!(args.contains(&"ControlMaster=auto".to_string()));
    assert!(args.contains(&"ControlPath=/tmp/homeboy-test-%h-%p-%r".to_string()));
    assert!(args.contains(&"ControlPersist=4h".to_string()));
    assert!(args.contains(&"BatchMode=yes".to_string()));
    assert!(args.contains(&"2222".to_string()));
    assert_eq!(args.last().map(String::as_str), Some("uptime"));
}

#[test]
fn managed_session_connect_builds_master_command() {
    let client = SshClient {
        host: "bastion.example.test".to_string(),
        user: "deploy".to_string(),
        port: 22,
        identity_file: Some("/tmp/key".to_string()),
        auth: Some(ManagedSshSession {
            control_path: "/tmp/homeboy-test-control".to_string(),
            persist: "10m".to_string(),
        }),
        is_local: false,
        env: HashMap::new(),
    };

    let args = client.build_session_connect_args().expect("args");

    assert!(args.contains(&"-M".to_string()));
    assert!(args.contains(&"-N".to_string()));
    assert!(args.contains(&"-f".to_string()));
    assert!(args.contains(&"ControlMaster=yes".to_string()));
    assert!(args.contains(&"ControlPath=/tmp/homeboy-test-control".to_string()));
    assert!(args.contains(&"ControlPersist=10m".to_string()));
    assert_eq!(
        args.last().map(String::as_str),
        Some("deploy@bastion.example.test")
    );
}

#[test]
fn test_from_server() {
    let server = Server {
        id: "local".to_string(),
        aliases: Vec::new(),
        host: "localhost".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        kind: Some("local".to_string()),
        auth: Some(super::super::ServerAuth {
            mode: ServerAuthMode::KeyPlusPasswordControlmaster,
            session: ServerSessionConfig {
                control_path: Some("/tmp/homeboy-local-%h-%p-%r".to_string()),
                persist: Some("5m".to_string()),
            },
        }),
        env: HashMap::new(),
        runner: None,
    };

    let client = SshClient::from_server(&server, "local").expect("client");

    assert!(client.is_local);
    assert_eq!(client.host, "localhost");
    assert_eq!(client.user, "tester");
    assert_eq!(
        client.auth.as_ref().map(|auth| auth.persist.as_str()),
        Some("5m")
    );
}

#[test]
fn test_connect_managed_session() {
    let client = local_managed_session_client();

    let output = client.connect_managed_session().expect("connect");

    assert!(output.live);
    assert_eq!(output.session.persist.as_deref(), Some("10m"));
    assert_eq!(output.exit_code, 0);
}

#[test]
fn test_check_managed_session() {
    let client = local_managed_session_client();

    let output = client.check_managed_session().expect("check");

    assert!(output.live);
    assert_eq!(
        output.session.control_path.as_deref(),
        Some("/tmp/homeboy-local-control")
    );
    assert_eq!(output.exit_code, 0);
}

#[test]
fn test_disconnect_managed_session() {
    let client = local_managed_session_client();

    let output = client.disconnect_managed_session().expect("disconnect");

    assert!(!output.live);
    assert_eq!(output.exit_code, 0);
}

#[test]
fn managed_session_connect_reports_per_command_fallback() {
    let client = local_managed_session_client();
    let output = client.with_per_command_connect_fallback(
        ManagedSshSessionOutput {
            session: client.output_session_config(),
            live: false,
            stdout: String::new(),
            stderr: "Connection closed by 192.0.96.181 port 22".to_string(),
            exit_code: 255,
        },
        CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: true,
            exit_code: 0,
            timed_out: false,
            child_resource: None,
        },
    );

    assert!(!output.live);
    assert_eq!(output.exit_code, 0);
    assert!(output.stderr.contains("Connection closed"));
    assert!(output.stderr.contains("per-command SSH succeeded"));
}

#[test]
fn test_execute_interactive() {
    let client = SshClient {
        host: "localhost".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        auth: None,
        is_local: true,
        env: HashMap::new(),
    };

    assert_eq!(client.execute_interactive(Some("true")), 0);
}

#[test]
fn test_execute_local_command_interactive() {
    assert_eq!(execute_local_command_interactive("true", None, None), 0);
}

#[test]
fn test_execute_local_command_passthrough() {
    let output = execute_local_command_passthrough("printf 'passthrough\\n'", None, None);

    assert!(output.success);
    assert_eq!(output.stdout, "passthrough\n");
}

fn local_managed_session_client() -> SshClient {
    SshClient {
        host: "localhost".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        auth: Some(ManagedSshSession {
            control_path: "/tmp/homeboy-local-control".to_string(),
            persist: "10m".to_string(),
        }),
        is_local: true,
        env: HashMap::new(),
    }
}

#[test]
fn test_execute_local_command_stderr_passthrough() {
    let output = execute_local_command_stderr_passthrough(
        "printf '{\"ok\":true}\n'; printf 'progress turn=1\n' >&2",
        None,
        None,
    );

    assert!(output.success);
    assert_eq!(output.stdout, "{\"ok\":true}\n");
    assert_eq!(output.stderr, "progress turn=1\n");
}

#[cfg(unix)]
#[test]
fn process_cleanup_kills_lingering_background_children() {
    let pid_file = std::env::temp_dir().join(format!(
        "homeboy-process-cleanup-{}.pid",
        uuid::Uuid::new_v4()
    ));
    let command = format!(
        "sh -c 'sleep 30 >/dev/null 2>&1 < /dev/null & echo $! > {}'",
        crate::core::engine::shell::quote_path(&pid_file.to_string_lossy())
    );

    let output = execute_local_command_in_dir(&command, None, None);

    assert!(output.success, "command failed: {}", output.stderr);
    let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
        .expect("pid file")
        .trim()
        .parse()
        .expect("pid");
    let _ = std::fs::remove_file(&pid_file);

    for _ in 0..20 {
        if !pid_is_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        !pid_is_alive(pid),
        "background child {pid} should be cleaned up"
    );
}

#[cfg(unix)]
fn pid_is_alive(pid: libc::pid_t) -> bool {
    u32::try_from(pid)
        .map(crate::core::process::pid_is_running)
        .unwrap_or(false)
}
