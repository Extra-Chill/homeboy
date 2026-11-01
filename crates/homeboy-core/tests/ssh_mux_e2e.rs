//! End-to-end: inside one mux scope, ssh exec, scp, and rsync to a local sshd
//! share a single authenticated connection, and the master is closed and its
//! socket removed when the scope ends — after success and after a failure.
//!
//! Skips when no runnable `sshd` is available. Lives in its own test binary
//! because it points `PATH` at ssh/scp wrappers that trust the fixture host key.

#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use homeboy_core::server::ssh_args::{client_option_args, SshArgOptions, SshPortFlag};
use homeboy_core::server::{ssh_mux, SshClient};

struct Sshd {
    child: Child,
    log: PathBuf,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Sshd {
    fn count(&self, needle: &str) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .matches(needle)
            .count()
    }

    fn wait_for(&self, needle: &str, at_least: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.count(needle) >= at_least {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}

fn run(command: &mut Command) {
    let status = command.status().expect("spawn fixture command");
    assert!(status.success(), "fixture command failed: {command:?}");
}

fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).expect("write wrapper");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn start_sshd(fixture: &Path, user: &str) -> Option<(Sshd, u16)> {
    let sshd = ["/usr/sbin/sshd", "/usr/bin/sshd"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())?;
    for key in ["host_key", "client_key"] {
        run(Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(fixture.join(key)));
    }
    std::fs::copy(
        fixture.join("client_key.pub"),
        fixture.join("authorized_keys"),
    )
    .expect("authorized_keys");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("free port")
        .port();
    let config = fixture.join("sshd_config");
    std::fs::write(
        &config,
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {dir}/host_key\n\
             AuthorizedKeysFile {dir}/authorized_keys\nAllowUsers {user}\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\n\
             UsePAM no\nStrictModes no\nPidFile {dir}/sshd.pid\nLogLevel VERBOSE\n",
            dir = fixture.display()
        ),
    )
    .expect("sshd_config");
    let log = fixture.join("sshd.log");
    let child = Command::new(sshd)
        .arg("-D")
        .arg("-f")
        .arg(&config)
        .arg("-E")
        .arg(&log)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let sshd = Sshd { child, log };
    sshd.wait_for("Server listening", 1).then_some((sshd, port))
}

#[test]
fn deploy_transports_share_one_owned_master_per_scope() {
    let user = std::env::var("USER").unwrap_or_default();
    let (Some(real_ssh), Some(real_scp), Some(_)) = (which("ssh"), which("scp"), which("rsync"))
    else {
        eprintln!("skipping: ssh, scp, or rsync not installed");
        return;
    };
    let fixture = tempfile::Builder::new()
        .prefix("hb-sshd-")
        .tempdir_in("/tmp")
        .expect("fixture dir");
    let fixture_path = fixture.path();
    let Some((sshd, port)) = start_sshd(fixture_path, &user) else {
        eprintln!("skipping: could not start an unprivileged sshd");
        return;
    };

    let host_key = std::fs::read_to_string(fixture_path.join("host_key.pub")).unwrap();
    let known_hosts = fixture_path.join("known_hosts");
    std::fs::write(&known_hosts, format!("[127.0.0.1]:{port} {host_key}")).unwrap();
    let bin = fixture_path.join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("ssh"),
        &format!(
            "#!/bin/sh\nexec {} -o UserKnownHostsFile={} \"$@\"\n",
            real_ssh.display(),
            known_hosts.display()
        ),
    );
    write_executable(
        &bin.join("scp"),
        &format!(
            "#!/bin/sh\nexec {} -S {} \"$@\"\n",
            real_scp.display(),
            bin.join("ssh").display()
        ),
    );
    let path = std::env::join_paths(
        std::iter::once(bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    std::env::set_var("PATH", path);

    let client = SshClient {
        host: "127.0.0.1".to_string(),
        user: user.clone(),
        port,
        identity_file: Some(fixture_path.join("client_key").display().to_string()),
        auth: None,
        is_local: false,
        env: HashMap::new(),
    };
    let remote = fixture_path.join("remote");
    let payload = fixture_path.join("payload");
    std::fs::create_dir_all(payload.join("tree")).unwrap();
    std::fs::write(payload.join("artifact.zip"), "archive").unwrap();
    std::fs::write(payload.join("tree/file.txt"), "synced").unwrap();

    // Success: exec + scp + rsync + exec share one connection.
    let socket_dir = {
        let scope = ssh_mux::scope();
        let socket_dir = scope.socket_dir().expect("mux active on unix");
        let mkdir = client.execute(&format!("mkdir -p {}", remote.display()));
        assert!(mkdir.success, "mkdir: {}", mkdir.stderr);

        let mut scp_args = vec!["-O".to_string()];
        scp_args.extend(client_option_args(
            &client,
            SshArgOptions {
                batch_mode: true,
                connect_timeout: true,
                port_flag: Some(SshPortFlag::Uppercase),
                ..SshArgOptions::default()
            },
        ));
        let scp = Command::new("scp")
            .args(&scp_args)
            .arg(payload.join("artifact.zip"))
            .arg(format!(
                "{user}@127.0.0.1:{}/artifact.zip",
                remote.display()
            ))
            .output()
            .unwrap();
        assert!(
            scp.status.success(),
            "scp: {}",
            String::from_utf8_lossy(&scp.stderr)
        );

        let mut ssh_cmd = vec!["ssh".to_string()];
        ssh_cmd.extend(client_option_args(
            &client,
            SshArgOptions {
                batch_mode: true,
                connect_timeout: true,
                port_flag: Some(SshPortFlag::Lowercase),
                ..SshArgOptions::default()
            },
        ));
        let rsync = Command::new("rsync")
            .args(["-a", "-e", &ssh_cmd.join(" ")])
            .arg(format!("{}/", payload.join("tree").display()))
            .arg(format!("{user}@127.0.0.1:{}/tree/", remote.display()))
            .output()
            .unwrap();
        assert!(
            rsync.status.success(),
            "rsync: {}",
            String::from_utf8_lossy(&rsync.stderr)
        );

        let verify = client.execute(&format!(
            "cat {0}/artifact.zip {0}/tree/file.txt",
            remote.display()
        ));
        assert!(verify.success, "verify: {}", verify.stderr);
        assert_eq!(verify.stdout.trim(), "archivesynced");
        assert_eq!(std::fs::read_dir(&socket_dir).unwrap().count(), 1);
        socket_dir
    };
    assert_eq!(sshd.count("Connection from"), 1, "one connection per scope");
    assert!(!socket_dir.exists(), "socket dir removed after success");
    assert!(
        sshd.wait_for("Disconnected from user", 1),
        "master exits after success"
    );

    // Failure: the scope still closes its master on the error path.
    let mut failed_socket_dir = None;
    let failed: Result<(), String> = (|| {
        let scope = ssh_mux::scope();
        failed_socket_dir = scope.socket_dir();
        let output = client.execute("exit 3");
        if output.success {
            return Ok(());
        }
        Err(format!("exit {}", output.exit_code))
    })();
    assert_eq!(failed, Err("exit 3".to_string()));
    assert_eq!(sshd.count("Connection from"), 2);
    assert!(!failed_socket_dir.expect("mux active").exists());
    assert!(
        sshd.wait_for("Disconnected from user", 2),
        "master exits after failure"
    );
}
