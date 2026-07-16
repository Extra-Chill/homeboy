use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::TempDir;

static SHARED_EMPTY_GIT_REPO_TEMPLATE: OnceLock<TempDir> = OnceLock::new();
static SHARED_COMMITTED_GIT_REPO_TEMPLATE: OnceLock<TempDir> = OnceLock::new();

/// An explicit executable selection for a hermetic test command.
///
/// Fixture commands never resolve `homeboy` through `PATH`: integration tests
/// select Cargo's binary and unit tests can select their current test binary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestBinary {
    CurrentTest,
    HomeboyFixture,
}

/// Isolated filesystem and process environment for one test.
///
/// Constructing this type does not mutate the parent process. Prefer
/// [`HermeticTestContext::command`] for subprocess tests. `HomeGuard` exists
/// only for legacy in-process tests whose dependencies still read environment
/// variables directly.
pub struct HermeticTestContext {
    root: TempDir,
    runtime: TempDir,
    invocation_runtime: TempDir,
}

impl HermeticTestContext {
    pub fn new() -> Self {
        let context = Self {
            root: exec_capable_tempdir(),
            runtime: exec_capable_tempdir(),
            invocation_runtime: short_invocation_tempdir(),
        };
        for path in [
            context.root().join(".config"),
            context.data_dir(),
            context.artifact_dir(),
            context.temp_dir(),
            context.daemon_dir(),
            context.runner_dir(),
        ] {
            fs::create_dir_all(path).expect("create hermetic test path");
        }
        context
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn home(&self) -> &Path {
        self.root()
    }

    pub fn config_dir(&self) -> PathBuf {
        self.home().join(".config/homeboy")
    }

    pub fn data_dir(&self) -> PathBuf {
        self.root().join("data/homeboy")
    }

    pub fn artifact_dir(&self) -> PathBuf {
        self.root().join("artifacts")
    }

    pub fn runtime_dir(&self) -> &Path {
        self.runtime.path()
    }

    pub fn temp_dir(&self) -> PathBuf {
        self.root().join("tmp")
    }

    pub fn daemon_dir(&self) -> PathBuf {
        self.config_dir().join("daemon")
    }

    pub fn runner_dir(&self) -> PathBuf {
        self.config_dir().join("runners")
    }

    pub fn binary_path(&self, binary: TestBinary) -> PathBuf {
        match binary {
            TestBinary::CurrentTest => std::env::current_exe().expect("current test executable"),
            TestBinary::HomeboyFixture => PathBuf::from(
                std::env::var_os("CARGO_BIN_EXE_homeboy")
                    .expect("CARGO_BIN_EXE_homeboy fixture binary"),
            ),
        }
    }

    /// Build a command whose Homeboy state is wholly owned by this context.
    pub fn command(&self, binary: TestBinary) -> Command {
        let mut command = Command::new(self.binary_path(binary));
        command
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.root().join(".config"))
            .env("XDG_DATA_HOME", self.root().join("data"))
            .env("HOMEBOY_ARTIFACT_ROOT", self.artifact_dir())
            .env("HOMEBOY_RUNTIME_TMPDIR", self.runtime_dir())
            .env("TMPDIR", self.temp_dir())
            .env(
                crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
                self.invocation_runtime.path(),
            )
            .env("HOMEBOY_NO_UPDATE_CHECK", "1");
        command
    }
}

impl Default for HermeticTestContext {
    fn default() -> Self {
        Self::new()
    }
}

pub struct HomeGuard {
    prior: Option<String>,
    prior_xdg_data_home: Option<String>,
    prior_artifact_root: Option<String>,
    prior_runtime_tmpdir: Option<String>,
    prior_invocation_runtime: Option<String>,
    prior_no_update_check: Option<String>,
    prior_daemon_binary_sha: Option<String>,
    context: HermeticTestContext,
    _guard: MutexGuard<'static, ()>,
}

/// A fixed, well-formed (64-hex) SHA the daemon uses in place of hashing the
/// running executable during tests. Hashing the multi-hundred-MB debug test
/// binary costs ~20s per daemon-state write; a stable placeholder keeps daemon
/// tests fast and deterministic. It is only honored via
/// `HOMEBOY_TEST_DAEMON_BINARY_SHA`, which no released binary sets.
const TEST_DAEMON_BINARY_SHA: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

pub struct AuditGuard {
    _guard: MutexGuard<'static, ()>,
    _home_guard: MutexGuard<'static, ()>,
}

pub struct AuditHomeGuard {
    home: HomeGuard,
    _guard: MutexGuard<'static, ()>,
}

pub struct ArtifactRootOverrideGuard;

impl ArtifactRootOverrideGuard {
    pub fn new(path: PathBuf) -> Self {
        crate::set_artifact_root_override(Some(path));
        Self
    }
}

impl Drop for ArtifactRootOverrideGuard {
    fn drop(&mut self) {
        crate::set_artifact_root_override(None);
    }
}

impl AuditGuard {
    pub fn new() -> Self {
        let home_guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
        let guard = audit_lock().lock().unwrap_or_else(|e| e.into_inner());
        Self {
            _guard: guard,
            _home_guard: home_guard,
        }
    }
}

impl AuditHomeGuard {
    pub fn new() -> Self {
        let home = HomeGuard::new();
        let guard = audit_lock().lock().unwrap_or_else(|e| e.into_inner());
        Self {
            _guard: guard,
            home,
        }
    }
}

impl HomeGuard {
    pub fn new() -> Self {
        let guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
        reset_cached_test_state();
        let prior = std::env::var("HOME").ok();
        let prior_xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
        let prior_artifact_root = std::env::var("HOMEBOY_ARTIFACT_ROOT").ok();
        let prior_runtime_tmpdir = std::env::var("HOMEBOY_RUNTIME_TMPDIR").ok();
        let prior_invocation_runtime =
            std::env::var(crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV).ok();
        let prior_no_update_check = std::env::var("HOMEBOY_NO_UPDATE_CHECK").ok();
        let prior_daemon_binary_sha =
            std::env::var(crate::daemon::DAEMON_BINARY_SHA_OVERRIDE_ENV).ok();
        // The isolated HOME hosts `~/.config/homeboy/extensions/**/*.sh`
        // capability scripts that tests execute. On `noexec`-`/tmp` hosts a
        // plain `TempDir::new()` lands the whole HOME on a `noexec` mount,
        // failing every capability-script test with exit 126 (#6760). Anchor
        // it (and the runtime tmpdir, which also hosts executables) on an
        // exec-capable root.
        let context = HermeticTestContext::new();
        std::env::set_var("HOME", context.home());
        // Preserve the legacy in-process defaults while the subprocess context
        // uses explicit paths. These tests exercise fallback path resolution.
        std::env::set_var("XDG_DATA_HOME", context.home().join(".local").join("share"));
        std::env::remove_var("HOMEBOY_ARTIFACT_ROOT");
        std::env::set_var("HOMEBOY_NO_UPDATE_CHECK", "1");
        std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", context.runtime_dir());
        crate::set_artifact_root_override(None);
        // Pin invocation runtime to a SHORT tempdir, isolated from `$TMPDIR`
        // and from the home tempdir (which itself can already live on a long
        // path on macOS, e.g. `/var/folders/<14>/T/.tmpXXXXXX/...`). Using
        // `/tmp` directly keeps tests within the platform `sockaddr_un`
        // budget regardless of host configuration.
        std::env::set_var(
            crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
            context.invocation_runtime.path(),
        );
        // Avoid hashing the giant debug test binary on every daemon-state write.
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
            context,
            _guard: guard,
        }
    }
}

/// Return a short-path tempdir suitable for the invocation runtime root.
///
/// Two competing constraints:
/// 1. The path must be **short** so runtime unix sockets stay within the
///    `sockaddr_un` budget (~104 bytes) even on macOS, where `$TMPDIR`
///    typically lives at a long `/var/folders/<14>/T/.tmpXXXXXX/` path.
/// 2. The path must be **exec-capable** — tests execute capability scripts
///    from runtime paths, so a `noexec` mount (common for `/tmp` on hardened
///    VPS hosts, containers, and CI sandboxes) causes deterministic exit-126
///    "Permission denied" failures (#6760).
///
/// Previously this anchored unconditionally to `/tmp`, which satisfies (1) but
/// silently breaks (2) on `noexec`-`/tmp` hosts. Instead we probe short
/// candidate roots for real exec capability and use the first that passes,
/// falling back to the default tempdir if none qualify.
fn short_invocation_tempdir() -> TempDir {
    #[cfg(unix)]
    {
        for base in short_tempdir_candidates() {
            let Ok(candidate) = tempfile::Builder::new()
                .prefix("hb-test-")
                .tempdir_in(&base)
            else {
                continue;
            };
            if dir_allows_exec(candidate.path()) {
                return candidate;
            }
            // Not exec-capable (e.g. `noexec` mount) — drop it and try the
            // next candidate rather than handing back a dir scripts can't run
            // from.
        }
    }
    TempDir::new().expect("invocation runtime tempdir")
}

/// Ordered short base directories to consider for the invocation runtime root.
///
/// Honors an explicit `$TMPDIR` first (respecting operator intent, e.g. a
/// dedicated exec-capable tmp), but only when it is short enough to keep unix
/// socket paths within budget. Then the conventional short system roots.
#[cfg(unix)]
fn short_tempdir_candidates() -> Vec<PathBuf> {
    // Leave generous headroom under the ~104-byte sockaddr_un limit for the
    // per-tempdir suffix, socket filename, and run-id segments appended later.
    const MAX_BASE_LEN: usize = 40;

    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut push_if_usable = |path: PathBuf| {
        if path.as_os_str().len() <= MAX_BASE_LEN && path.is_dir() && !candidates.contains(&path) {
            candidates.push(path);
        }
    };

    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        push_if_usable(PathBuf::from(tmpdir));
    }
    push_if_usable(PathBuf::from("/tmp"));
    push_if_usable(PathBuf::from("/var/tmp"));
    push_if_usable(PathBuf::from("/dev/shm"));
    candidates
}

/// Probe whether files created under `dir` can actually be executed.
///
/// A `noexec` mount is invisible in file metadata — the only reliable check is
/// to write a trivial executable and run it. Returns `false` on any failure so
/// the caller falls through to the next candidate.
#[cfg(unix)]
fn dir_allows_exec(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let probe = dir.join(".hb-exec-probe.sh");
    if fs::write(&probe, "#!/bin/sh\nexit 0\n").is_err() {
        return false;
    }
    if fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).is_err() {
        let _ = fs::remove_file(&probe);
        return false;
    }
    let allowed = Command::new(&probe)
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    let _ = fs::remove_file(&probe);
    allowed
}

/// Create a tempdir that is guaranteed exec-capable where possible.
///
/// Tests that write a script and then run it (e.g. capability parser scripts)
/// must not land on a `noexec` filesystem. The default `tempfile::tempdir()`
/// honors `$TMPDIR`, which on hardened VPS hosts / containers / CI sandboxes is
/// frequently a `noexec` `/tmp` — producing deterministic exit-126
/// "Permission denied" failures unrelated to the behavior under test (#6760).
///
/// This probes exec-capable roots (honoring an exec-capable `$TMPDIR` first,
/// then `/tmp`, `/var/tmp`, `/dev/shm`) and returns the first that can actually
/// run a file. Falls back to the default tempdir when no candidate qualifies
/// (e.g. non-Unix), so callers keep working on hosts where `/tmp` is fine.
pub fn exec_capable_tempdir() -> TempDir {
    #[cfg(unix)]
    {
        let mut roots: Vec<PathBuf> = Vec::new();
        let push = |path: PathBuf, roots: &mut Vec<PathBuf>| {
            if path.is_dir() && !roots.contains(&path) {
                roots.push(path);
            }
        };
        if let Some(tmpdir) = std::env::var_os("TMPDIR") {
            push(PathBuf::from(tmpdir), &mut roots);
        }
        for fixed in ["/tmp", "/var/tmp", "/dev/shm"] {
            push(PathBuf::from(fixed), &mut roots);
        }

        for base in roots {
            let Ok(candidate) = tempfile::Builder::new()
                .prefix("hb-test-")
                .tempdir_in(&base)
            else {
                continue;
            };
            if dir_allows_exec(candidate.path()) {
                return candidate;
            }
        }
    }
    TempDir::new().expect("exec-capable tempdir")
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match &self.prior_xdg_data_home {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        match &self.prior_artifact_root {
            Some(value) => std::env::set_var("HOMEBOY_ARTIFACT_ROOT", value),
            None => std::env::remove_var("HOMEBOY_ARTIFACT_ROOT"),
        }
        match &self.prior_runtime_tmpdir {
            Some(value) => std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", value),
            None => std::env::remove_var("HOMEBOY_RUNTIME_TMPDIR"),
        }
        crate::set_artifact_root_override(None);
        match &self.prior_invocation_runtime {
            Some(value) => std::env::set_var(
                crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
                value,
            ),
            None => {
                std::env::remove_var(crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV)
            }
        }
        match &self.prior_no_update_check {
            Some(value) => std::env::set_var("HOMEBOY_NO_UPDATE_CHECK", value),
            None => std::env::remove_var("HOMEBOY_NO_UPDATE_CHECK"),
        }
        match &self.prior_daemon_binary_sha {
            Some(value) => std::env::set_var(crate::daemon::DAEMON_BINARY_SHA_OVERRIDE_ENV, value),
            None => std::env::remove_var(crate::daemon::DAEMON_BINARY_SHA_OVERRIDE_ENV),
        }
        reset_cached_test_state();
    }
}

pub fn with_isolated_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
    let home = HomeGuard::new();
    body(&home.context.root)
}

pub fn with_isolated_audit_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
    let guard = AuditHomeGuard::new();
    body(&guard.home.context.root)
}

/// Additional cache-reset hooks registered by layers above core (e.g. the CLI
/// crate resets its entity-suggestion cache). Core's test isolation resets its
/// own caches and then invokes these, so higher layers don't need core to know
/// about their internals.
static TEST_CACHE_RESET_HOOKS: std::sync::Mutex<Vec<fn()>> = std::sync::Mutex::new(Vec::new());

/// Register a cache-reset hook invoked whenever a hermetic test home is set up.
/// Called by higher layers (CLI) at test startup.
pub fn register_test_cache_reset_hook(hook: fn()) {
    let mut hooks = TEST_CACHE_RESET_HOOKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !hooks
        .iter()
        .any(|existing| *existing as usize == hook as usize)
    {
        hooks.push(hook);
    }
}

fn reset_cached_test_state() {
    crate::defaults::reset_config_cache_for_test();
    let hooks = TEST_CACHE_RESET_HOOKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    for hook in hooks {
        hook();
    }
}

pub fn write_source_extension(home: &std::path::Path, id: &str, file_extension: &str) {
    let extension_dir = home.join(".config/homeboy/extensions").join(id);
    std::fs::create_dir_all(&extension_dir).expect("extension dir");
    std::fs::write(
        extension_dir.join(format!("{id}.json")),
        serde_json::json!({
            "name": id,
            "version": "0.0.0",
            "provides": {
                "file_extensions": [file_extension]
            },
            "scripts": {
                "fingerprint": "fingerprint.sh"
            }
        })
        .to_string(),
    )
    .expect("source extension manifest");
    std::fs::write(
        extension_dir.join("fingerprint.sh"),
        "#!/usr/bin/env sh\nexit 1\n",
    )
    .expect("fingerprint script");

    if matches!(file_extension, "rs" | "fixture") {
        std::fs::write(extension_dir.join("grammar.toml"), minimal_source_grammar())
            .expect("source grammar");
    }
}

pub fn shared_git_repo_fixture(name: &str) -> (TempDir, PathBuf) {
    shared_git_repo_fixture_from_template(name, shared_empty_git_repo_template())
}

pub fn shared_committed_git_repo_fixture(name: &str) -> (TempDir, PathBuf) {
    shared_git_repo_fixture_from_template(name, shared_committed_git_repo_template())
}

fn shared_git_repo_fixture_from_template(name: &str, template: &Path) -> (TempDir, PathBuf) {
    let temp = TempDir::new().expect("git fixture tempdir");
    let repo = temp.path().join(name);
    copy_dir_all(template, &repo).expect("copy git fixture template");
    (temp, repo)
}

fn shared_empty_git_repo_template() -> &'static Path {
    SHARED_EMPTY_GIT_REPO_TEMPLATE
        .get_or_init(|| {
            let temp = TempDir::new().expect("git template tempdir");
            run_git_template_command(temp.path(), &["init", "-q"]);
            temp
        })
        .path()
}

fn shared_committed_git_repo_template() -> &'static Path {
    SHARED_COMMITTED_GIT_REPO_TEMPLATE
        .get_or_init(|| {
            let temp = TempDir::new().expect("committed git template tempdir");
            run_git_template_command(temp.path(), &["init", "-q"]);
            std::fs::write(temp.path().join("README.md"), "# homeboy test fixture\n")
                .expect("git template readme");
            run_git_template_command(temp.path(), &["add", "README.md"]);
            run_git_template_command(temp.path(), &["commit", "-q", "-m", "test fixture"]);
            temp
        })
        .path()
}

fn copy_dir_all(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub fn run_git_fixture_command(repo: &Path, args: &[&str]) {
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

fn run_git_template_command(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "homeboy-test")
        .env("GIT_AUTHOR_EMAIL", "homeboy-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "homeboy-test")
        .env("GIT_COMMITTER_EMAIL", "homeboy-test@example.invalid")
        .output()
        .expect("git template command");
    assert!(
        output.status.success(),
        "git template command {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn git_fixture_output(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout)
        .expect("git fixture output utf8")
        .trim()
        .to_string()
}

fn minimal_source_grammar() -> &'static str {
    r#"
[language]
id = "source"
extensions = ["rs", "fixture"]

[comments]
line = ["//"]
block = [["/*", "*/"]]
doc = ["///", "//!"]

[strings]
quotes = ['"']
escape = "\\"

[blocks]
open = "{"
close = "}"

[fingerprint]
keywords = ["fn", "let", "if", "for", "return", "true", "false", "pub", "struct", "impl", "trait", "Self", "Result", "String", "bool", "i32", "usize"]
skip_calls = ["if", "for", "return", "println", "write", "assert"]
contract_method_names = []
contract_type_hints = []
registration_concepts = ["macro_invocation"]
registration_skip_names = ["println", "assert", "write"]
registration_skip_prefixes = ["test"]

[fingerprint.namespace_derivation]
prefix = "crate::"
strip_leading_segments = 1
separator = "::"
include_file_stem_when_root = true

[patterns.function]
regex = '^\s*(pub(?:\(crate\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)\s*\(([^)]*)\)'
context = "any"

[patterns.function.captures]
visibility = 1
name = 2
params = 3

[patterns.struct]
regex = '^\s*(pub(?:\(crate\))?\s+)?(struct|enum|trait)\s+(\w+)'
context = "top_level"

[patterns.struct.captures]
visibility = 1
kind = 2
name = 3

[patterns.import]
regex = '^use\s+([\w:]+(?:::\{[^}]+\})?)\s*;'
context = "top_level"

[patterns.import.captures]
path = 1

[patterns.impl_block]
regex = '^\s*impl(?:<[^>]*>)?\s+(?:(\w+)\s+for\s+)?(\w+)'
context = "any"

[patterns.impl_block.captures]
trait_name = 1
type_name = 2

[patterns.test_attribute]
regex = '#\[test\]'
context = "any"

[patterns.cfg_test]
regex = '#\[cfg\(test\)\]'
context = "any"
"#
}

pub fn home_env_guard() -> MutexGuard<'static, ()> {
    env_lock()
}

/// Serializes tests that mutate or capture process-global environment state.
pub fn env_lock() -> MutexGuard<'static, ()> {
    home_lock().lock().unwrap_or_else(|e| e.into_inner())
}

fn home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn audit_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Spin up a single-shot localhost HTTP server returning `status` once, used to
/// probe public-artifact-URL reachability. Returns the base URL ending in
/// `/homeboy`. Shared by `runs` and `bench` artifact-viewer tests.
pub fn serve_public_artifact_base_once(status: u16) -> String {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind public artifact server");
    let addr = listener.local_addr().expect("server address");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept public artifact probe");
        let mut buffer = [0; 1024];
        let _ = stream.read(&mut buffer);
        let status_text = if status == 200 { "OK" } else { "Not Found" };
        let body = if status == 200 { "{}" } else { "missing" };
        write!(
            stream,
            "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("write public artifact response");
    });
    format!("http://{addr}/homeboy")
}
