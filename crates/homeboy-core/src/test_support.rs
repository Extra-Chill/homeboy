use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::TempDir;

const TEST_DAEMON_BINARY_SHA: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

pub(crate) struct HomeGuard {
    prior: Option<String>,
    prior_xdg_data_home: Option<String>,
    prior_artifact_root: Option<String>,
    prior_runtime_tmpdir: Option<String>,
    prior_invocation_runtime: Option<String>,
    prior_no_update_check: Option<String>,
    prior_daemon_binary_sha: Option<String>,
    home: TempDir,
    _runtime: TempDir,
    _invocation_runtime: TempDir,
    _guard: MutexGuard<'static, ()>,
}

pub(crate) struct AuditHomeGuard {
    pub(crate) home: HomeGuard,
    _guard: MutexGuard<'static, ()>,
}

pub(crate) struct AuditGuard {
    _home_guard: MutexGuard<'static, ()>,
    _audit_guard: MutexGuard<'static, ()>,
}

impl AuditGuard {
    pub(crate) fn new() -> Self {
        let home_guard = env_lock();
        let audit_guard = audit_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Self {
            _home_guard: home_guard,
            _audit_guard: audit_guard,
        }
    }
}

impl HomeGuard {
    pub(crate) fn new() -> Self {
        let guard = env_lock();
        crate::defaults::reset_config_cache_for_test();
        let home = exec_capable_tempdir();
        let runtime = exec_capable_tempdir();
        let invocation_runtime = short_invocation_tempdir();
        let prior = std::env::var("HOME").ok();
        let prior_xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
        let prior_artifact_root = std::env::var("HOMEBOY_ARTIFACT_ROOT").ok();
        let prior_runtime_tmpdir = std::env::var("HOMEBOY_RUNTIME_TMPDIR").ok();
        let prior_invocation_runtime =
            std::env::var(crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV).ok();
        let prior_no_update_check = std::env::var("HOMEBOY_NO_UPDATE_CHECK").ok();
        let prior_daemon_binary_sha =
            std::env::var(crate::daemon::DAEMON_BINARY_SHA_OVERRIDE_ENV).ok();

        for path in [
            home.path().join(".config"),
            home.path().join(".local/share"),
            home.path().join("artifacts"),
            home.path().join("tmp"),
            home.path().join(".config/homeboy/daemon"),
            home.path().join(".config/homeboy/runners"),
        ] {
            fs::create_dir_all(path).expect("create hermetic test path");
        }

        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_DATA_HOME", home.path().join(".local/share"));
        std::env::remove_var("HOMEBOY_ARTIFACT_ROOT");
        std::env::set_var("HOMEBOY_NO_UPDATE_CHECK", "1");
        std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", runtime.path());
        crate::set_artifact_root_override(None);
        std::env::set_var(
            crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
            invocation_runtime.path(),
        );
        std::env::set_var(
            crate::daemon::DAEMON_BINARY_SHA_OVERRIDE_ENV,
            TEST_DAEMON_BINARY_SHA,
        );

        Self {
            prior,
            prior_xdg_data_home,
            prior_artifact_root,
            prior_runtime_tmpdir,
            prior_invocation_runtime,
            prior_no_update_check,
            prior_daemon_binary_sha,
            home,
            _runtime: runtime,
            _invocation_runtime: invocation_runtime,
            _guard: guard,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        restore_env("HOME", &self.prior);
        restore_env("XDG_DATA_HOME", &self.prior_xdg_data_home);
        restore_env("HOMEBOY_ARTIFACT_ROOT", &self.prior_artifact_root);
        restore_env("HOMEBOY_RUNTIME_TMPDIR", &self.prior_runtime_tmpdir);
        restore_env(
            crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
            &self.prior_invocation_runtime,
        );
        restore_env("HOMEBOY_NO_UPDATE_CHECK", &self.prior_no_update_check);
        restore_env(
            crate::daemon::DAEMON_BINARY_SHA_OVERRIDE_ENV,
            &self.prior_daemon_binary_sha,
        );
        crate::set_artifact_root_override(None);
        crate::defaults::reset_config_cache_for_test();
    }
}

pub(crate) fn with_isolated_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
    let home = HomeGuard::new();
    body(&home.home)
}

pub(crate) fn with_isolated_audit_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
    let home = HomeGuard::new();
    let guard = audit_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let audit_home = AuditHomeGuard {
        home,
        _guard: guard,
    };
    body(&audit_home.home.home)
}

pub(crate) fn home_env_guard() -> MutexGuard<'static, ()> {
    env_lock()
}

pub(crate) fn env_lock() -> MutexGuard<'static, ()> {
    home_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

pub(crate) fn exec_capable_tempdir() -> TempDir {
    TempDir::new().expect("exec-capable test tempdir")
}

fn short_invocation_tempdir() -> TempDir {
    tempfile::Builder::new()
        .prefix("hb-test-")
        .tempdir_in("/tmp")
        .unwrap_or_else(|_| TempDir::new().expect("invocation runtime tempdir"))
}

pub(crate) fn run_git_fixture_command(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git fixture command");
    assert!(
        output.status.success(),
        "git fixture command {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(crate) fn git_fixture_output(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git fixture command");
    assert!(
        output.status.success(),
        "git fixture command {:?} failed",
        args
    );
    String::from_utf8(output.stdout)
        .expect("git fixture output utf8")
        .trim()
        .to_string()
}

pub(crate) fn write_source_extension(home: &Path, id: &str, file_extension: &str) {
    let extension_dir = home.join(".config/homeboy/extensions").join(id);
    fs::create_dir_all(&extension_dir).expect("extension dir");
    fs::write(
        extension_dir.join(format!("{id}.json")),
        serde_json::json!({
            "name": id,
            "version": "0.0.0",
            "provides": { "file_extensions": [file_extension] },
            "scripts": { "fingerprint": "fingerprint.sh" }
        })
        .to_string(),
    )
    .expect("source extension manifest");
    fs::write(
        extension_dir.join("fingerprint.sh"),
        "#!/usr/bin/env sh\nexit 1\n",
    )
    .expect("fingerprint script");
}

pub(crate) fn serve_public_artifact_base_once(status: u16) -> String {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind public artifact server");
    let address = listener.local_addr().expect("server address");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept public artifact probe");
        let mut buffer = [0; 1024];
        let _ = stream.read(&mut buffer);
        let text = if status == 200 { "OK" } else { "Not Found" };
        let body = if status == 200 { "{}" } else { "missing" };
        write!(
            stream,
            "HTTP/1.1 {status} {text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("write public artifact response");
    });
    format!("http://{address}/homeboy")
}

fn restore_env(name: &str, value: &Option<String>) {
    match value {
        Some(value) => std::env::set_var(name, value),
        None => std::env::remove_var(name),
    }
}

fn home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn audit_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
