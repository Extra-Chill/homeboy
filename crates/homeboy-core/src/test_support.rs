use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
};

use tempfile::TempDir;
use uuid::Uuid;

use crate::api_jobs::{
    ControllerJobState, ControllerJobSubmissionOutcome, Job, JobEvent, JobEventKind, JobStore,
    RemoteRunnerJobRequest, RemoteRunnerJobResult,
};
use crate::daemon::controller_job_driver::{ControllerJobDriver, ControllerJobHandle};
use crate::error::{Error, Result};

pub fn save_test_server(id: &str, host: &str) -> Result<()> {
    crate::server::save(&crate::server::Server {
        id: id.to_string(),
        aliases: Vec::new(),
        host: host.to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        kind: None,
        auth: None,
        env: std::collections::HashMap::new(),
        runner: None,
    })
}

/// A real in-memory controller job boundary for domain-driver tests.
///
/// The harness keeps construction internals out of the production API while
/// letting feature crates execute drivers against durable checkpoint,
/// progress, and cancellation behavior.
pub struct ControllerJobHarness {
    store: JobStore,
    job_id: Uuid,
    driver: Arc<dyn ControllerJobDriver>,
}

impl ControllerJobHarness {
    pub fn new(driver: Arc<dyn ControllerJobDriver>, request: serde_json::Value) -> Result<Self> {
        driver.validate_secret_references(&request)?;
        let public_request = driver.public_request(&request)?;
        let request_digest = crate::daemon::hex_digest(&request)?;
        let store = JobStore::default();
        let outcome = store.admit_controller_job(
            format!("controller.{}", driver.job_type()),
            format!("test-controller-job:{}", Uuid::new_v4()),
            ControllerJobState {
                job_type: driver.job_type().to_string(),
                version: driver.version(),
                linked_durable_run_id: driver
                    .linked_durable_run_id(&request)
                    .as_deref()
                    .map(str::trim)
                    .filter(|run_id| !run_id.is_empty())
                    .map(str::to_string),
                request,
                public_request,
                request_digest,
                active_idempotency_key: None,
                checkpoint: None,
                cancellation_requested: false,
                cancellation_reason: None,
                execution_claim_id: None,
                recovery_attempted: false,
            },
        )?;
        let ControllerJobSubmissionOutcome::Submitted(job_id) = outcome else {
            return Err(Error::internal_unexpected(
                "unique controller test job was unexpectedly replayed",
            ));
        };
        store.claim_controller_execution(job_id, false)?;
        Ok(Self {
            store,
            job_id,
            driver,
        })
    }

    pub fn handle(&self) -> ControllerJobHandle {
        ControllerJobHandle::new(self.store.handle(self.job_id), Arc::clone(&self.driver))
    }

    pub fn job(&self) -> Result<Job> {
        self.store.get(self.job_id)
    }

    pub fn events(&self) -> Result<Vec<JobEvent>> {
        self.store.events(self.job_id)
    }

    pub fn checkpoint(&self) -> Result<Option<serde_json::Value>> {
        Ok(self.store.controller_job_state(self.job_id)?.checkpoint)
    }

    /// The driver-declared durable-run linkage persisted at admission.
    pub fn linked_durable_run_id(&self) -> Result<Option<String>> {
        Ok(self
            .store
            .controller_job_state(self.job_id)?
            .linked_durable_run_id)
    }

    pub fn request_cancellation(&self, reason: impl Into<String>) -> Result<Job> {
        self.store
            .request_controller_cancellation(self.job_id, reason.into())
    }
}

/// The durable-run fixture one test programs for a linked agent-task run.
#[derive(Clone)]
pub enum AgentTaskTerminalRunFixture {
    Terminal(crate::api_jobs::JobStatus),
    Active,
}

/// A deterministic agent-task terminal-recovery provider shared by every test
/// in the process.
///
/// The provider registry is process-global, so one shared fixture store is
/// registered exactly once; each test programs only its own unique run ids and
/// parallel tests cannot observe each other's resolutions.
pub struct AgentTaskTerminalRunFixtures {
    runs: Mutex<std::collections::HashMap<String, AgentTaskTerminalRunFixture>>,
}

impl AgentTaskTerminalRunFixtures {
    /// Install the shared provider and return its fixture store.
    pub fn install() -> &'static Self {
        static FIXTURES: OnceLock<AgentTaskTerminalRunFixtures> = OnceLock::new();
        FIXTURES.get_or_init(|| {
            crate::api_jobs::agent_task_terminal_recovery::register_agent_task_terminal_recovery_provider(
                Box::new(SharedProvider),
            );
            AgentTaskTerminalRunFixtures {
                runs: Mutex::new(std::collections::HashMap::new()),
            }
        })
    }

    /// Program `run_id` as a terminal agent-task run recovering `status`.
    pub fn terminal_run(&self, run_id: &str, status: crate::api_jobs::JobStatus) {
        self.runs
            .lock()
            .expect("terminal-run fixtures lock")
            .insert(
                run_id.to_string(),
                AgentTaskTerminalRunFixture::Terminal(status),
            );
    }

    /// Program `run_id` as a live (non-terminal) agent-task run.
    pub fn active_run(&self, run_id: &str) {
        self.runs
            .lock()
            .expect("terminal-run fixtures lock")
            .insert(run_id.to_string(), AgentTaskTerminalRunFixture::Active);
    }

    /// Remove one programmed run.
    pub fn remove(&self, run_id: &str) {
        self.runs
            .lock()
            .expect("terminal-run fixtures lock")
            .remove(run_id);
    }

    fn get(&self, run_id: &str) -> Option<AgentTaskTerminalRunFixture> {
        self.runs
            .lock()
            .expect("terminal-run fixtures lock")
            .get(run_id)
            .cloned()
    }
}

struct SharedProvider;

impl crate::api_jobs::agent_task_terminal_recovery::AgentTaskTerminalRecoveryProvider
    for SharedProvider
{
    fn recovered_terminal_agent_task_job(
        &self,
        run_id: &str,
    ) -> Option<crate::api_jobs::RecoveredTerminalJob> {
        let AgentTaskTerminalRunFixture::Terminal(status) =
            AgentTaskTerminalRunFixtures::install().get(run_id)?
        else {
            return None;
        };
        Some(
            crate::api_jobs::agent_task_terminal_recovery::recovered_terminal_job(
                status,
                serde_json::json!({
                    "kind": "test_agent_task_aggregate",
                    "run_id": run_id,
                    "status": status,
                }),
                run_id.to_string(),
                Vec::new(),
            ),
        )
    }

    fn linked_durable_run_state(
        &self,
        run_id: &str,
    ) -> Option<crate::api_jobs::DaemonLinkedDurableRunState> {
        match AgentTaskTerminalRunFixtures::install().get(run_id)? {
            AgentTaskTerminalRunFixture::Terminal(_) => {
                Some(crate::api_jobs::DaemonLinkedDurableRunState::Terminal)
            }
            AgentTaskTerminalRunFixture::Active => {
                Some(crate::api_jobs::DaemonLinkedDurableRunState::Active)
            }
        }
    }
}

const TEST_DAEMON_NAMESPACE_ENV: &str = "HOMEBOY_TEST_DAEMON_NAMESPACE";
/// Test-only contract between the hermetic context and the Cargo test runner.
/// The runner owns and reaps this process group when the test binary is
/// cancelled, so a daemon must not create a detached session that escapes it.
pub const TEST_KEEP_DAEMON_IN_PROCESS_GROUP_ENV: &str = "HOMEBOY_TEST_KEEP_DAEMON_IN_PROCESS_GROUP";

static SHARED_CONTROLLER_RUNTIME_FIXTURE: OnceLock<TempDir> = OnceLock::new();
#[cfg(unix)]
static SHARED_CONTROLLER_RUNTIME_VERSION_FIXTURE: OnceLock<PathBuf> = OnceLock::new();
static SHARED_HOMEBOY_CONTROLLER_RUNTIME_FIXTURE: OnceLock<TempDir> = OnceLock::new();
/// Destinations whose controller fixture bytes this process has already
/// published. Keeps the copy to at most once per process per fixture path —
/// including a path inherited from a parent test process — and serializes
/// concurrent materialization so two threads never copy the same binary twice.
static PUBLISHED_CONTROLLER_FIXTURES: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
/// File name every controller-runtime fixture is published under. Reading it
/// back off the destination path is what lets `ensure_test_controller_fixture`
/// tell *our* fixture apart from a binary a test chose for itself.
const CONTROLLER_FIXTURE_FILE_NAME: &str = "homeboy-controller-fixture";
static EXEC_CAPABLE_TEMP_BASE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
static SHORT_EXEC_CAPABLE_TEMP_BASE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
/// Runs the leaked-tempdir sweep exactly once per test process.
static LEAKED_TEMPDIR_SWEEP: OnceLock<()> = OnceLock::new();

// `tempfile::Builder` appends six random ASCII characters by default. Keep the
// base acceptance calculation aligned with the allocated directory shape.
#[cfg(unix)]
const TEMPFILE_RANDOM_SUFFIX_BYTES: usize = 6;
#[cfg(unix)]
const TEST_INVOCATION_ID: &str = "0123456789";

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

    /// Explicit roots for in-process services that support dependency
    /// injection. Unlike `HomeGuard`, constructing these does not mutate the
    /// parent process or serialize peer tests.
    pub fn path_roots(&self) -> crate::paths::PathRoots {
        crate::paths::PathRoots::new(self.config_dir(), self.data_dir(), self.artifact_dir())
    }

    pub fn runtime_dir(&self) -> &Path {
        self.runtime.path()
    }

    /// Short runtime root inherited by fixture subprocesses for invocation
    /// state and socket-capable workload paths.
    pub fn invocation_runtime_dir(&self) -> &Path {
        self.invocation_runtime.path()
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
        test_binary_path(binary)
    }

    /// Build a command whose Homeboy state is wholly owned by this context.
    pub fn command(&self, binary: TestBinary) -> Command {
        let mut command = Command::new(self.binary_path(binary));
        command
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.root().join(".config"))
            .env("XDG_DATA_HOME", self.root().join("data"))
            // These explicit overrides take precedence over HOME/XDG. Always
            // replace inherited operator roots so daemon discovery and runner
            // source leases remain inside this test namespace.
            .env(crate::paths::HOMEBOY_DATA_DIR_ENV, self.data_dir())
            .env(crate::paths::DAEMON_STATE_DIR_ENV, self.daemon_dir())
            .env(TEST_DAEMON_NAMESPACE_ENV, self.daemon_dir())
            .env(TEST_KEEP_DAEMON_IN_PROCESS_GROUP_ENV, "1")
            .env("HOMEBOY_ARTIFACT_ROOT", self.artifact_dir())
            .env("HOMEBOY_RUNTIME_TMPDIR", self.runtime_dir())
            .env("TMPDIR", self.temp_dir())
            .env("TEMP", self.temp_dir())
            .env("TMP", self.temp_dir())
            .env(
                crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
                self.invocation_runtime.path(),
            )
            // Lab transport belongs to the runner job that launched this test,
            // never to fixture subprocesses unless a test explicitly injects it.
            .env_remove(crate::observation::SOURCE_SNAPSHOT_METADATA_ENV)
            .env_remove(crate::observation::LAB_OFFLOAD_METADATA_ENV)
            .env("HOMEBOY_NO_UPDATE_CHECK", "1");
        command
    }

    /// Build an isolated command with the deterministic controller-runtime
    /// fixture required by tests that submit or resume agent-task work.
    pub fn controller_runtime_command(&self, binary: TestBinary) -> Command {
        let mut command = self.command(binary);
        command
            .env(
                crate::daemon::DAEMON_BINARY_SHA_OVERRIDE_ENV,
                TEST_DAEMON_BINARY_SHA,
            )
            // Destination and source travel together. The child materializes
            // the fixture itself the first time it reads the contract, so this
            // parent never copies a binary the child may not even need.
            .env(
                crate::controller_runtime::TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV,
                test_controller_fixture_path(binary),
            )
            .env(
                crate::controller_runtime::TEST_CONTROLLER_RUNTIME_SOURCE_ENV,
                test_controller_fixture_source(binary),
            )
            .env(
                crate::controller_runtime::TEST_CONTROLLER_RUNTIME_IDENTITY_ENV,
                crate::build_identity::current().display,
            )
            .env(
                crate::controller_runtime::TEST_CONTROLLER_RUNTIME_USE_ENV,
                "1",
            );
        command
    }
}

impl Default for HermeticTestContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Overrides [`DEFAULT_HERMETIC_SUBPROCESS_BUDGET`] for hosts that are slower
/// than the budget assumes. It bounds a *stuck* child, not a slow one, so the
/// default is deliberately far above any healthy invocation.
pub const HERMETIC_SUBPROCESS_BUDGET_ENV: &str = "HOMEBOY_TEST_SUBPROCESS_BUDGET_SECS";

/// Per-invocation ceiling for a hermetic subprocess.
///
/// The slowest healthy invocation in this suite is a cold controller-runtime
/// pin — a hash, a copy, and a re-exec of a multi-hundred-megabyte unoptimized
/// binary — which costs single-digit seconds even on a contended runner. Three
/// minutes is generous enough that crossing it means *blocked*, not *slow*.
const DEFAULT_HERMETIC_SUBPROCESS_BUDGET: Duration = Duration::from_secs(180);

/// Cadence at which a still-running child reports liveness evidence. The
/// libtest harness warns at 60s, so a 30s heartbeat guarantees at least one
/// diagnostic snapshot lands before a human reads that warning.
const HERMETIC_SUBPROCESS_HEARTBEAT: Duration = Duration::from_secs(30);

/// Run a hermetic subprocess to completion under a bounded wait.
///
/// `Command::output` waits on pipe EOF with no ceiling: a child that blocks —
/// or that exits while a background descendant retains the inherited pipes —
/// hangs the test thread forever. libtest cannot interrupt a blocked thread, so
/// the whole gate dies on its outer timeout with `failed: 0` and no test name.
/// A 25-minute CI failure that names nothing is indistinguishable from a build
/// break, which is how the same hang survived two fix attempts (#10687).
///
/// This kills the child's process tree once the budget elapses and panics with
/// the evidence needed to tell those cases apart: elapsed time, argv, pid,
/// whether the child produced *any* output before the kill, and the last
/// observed process-tree state. The failure is then attributed to one named
/// test, and every other test in the binary still runs and still reports.
///
/// Prefer this over `Command::output`/`wait_with_output` in every subprocess
/// test; it is the test-side counterpart of the `unbounded_output_capture`
/// audit rule.
pub fn bounded_output(mut command: Command) -> Output {
    let argv = rendered_argv(&command);
    let budget = hermetic_subprocess_budget();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Isolate the process group so a stuck child's descendants are reachable
    // for termination; without it a background grandchild keeps the pipes open
    // and the capture-reader join strands even after the root dies.
    crate::engine::command::isolate_process_tree(&mut command);

    let started = Instant::now();
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn hermetic subprocess `{argv}`: {error}"));
    let pid = child.id();

    let last_snapshot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let supervised = {
        let last_snapshot = Arc::clone(&last_snapshot);
        let argv = argv.clone();
        crate::engine::command::wait_with_bounded_output_supervised(
            &mut child,
            crate::engine::command::DEFAULT_CAPTURE_LIMIT_BYTES,
            budget,
            HERMETIC_SUBPROCESS_HEARTBEAT,
            || false,
            move |elapsed, tail| {
                let snapshot = process_tree_snapshot(pid);
                eprintln!(
                    "hermetic subprocess pid {pid} still running after {:.1}s (budget {:.0}s): {argv}\n  output so far: {}\n  process tree:\n{snapshot}",
                    elapsed.as_secs_f64(),
                    budget.as_secs_f64(),
                    if tail.trim().is_empty() {
                        "<none>".to_string()
                    } else {
                        tail.trim().to_string()
                    },
                );
                *last_snapshot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(snapshot);
                Ok(())
            },
        )
    }
    .unwrap_or_else(|error| panic!("supervise hermetic subprocess `{argv}` (pid {pid}): {error}"));

    let elapsed = started.elapsed();
    if supervised.termination == crate::engine::command::SupervisedCommandTermination::Completed {
        return supervised.output.into_output();
    }

    let output = supervised.output;
    let snapshot = last_snapshot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(|| "    (no heartbeat snapshot was taken)".to_string());
    let produced_output =
        output.capture.stdout.bytes_seen > 0 || output.capture.stderr.bytes_seen > 0;
    panic!(
        "hermetic subprocess did not finish within its {:.0}s budget (terminated: {:?})\n\
         \x20 argv:      {argv}\n\
         \x20 pid:       {pid}\n\
         \x20 elapsed:   {:.1}s\n\
         \x20 budget:    {:.0}s (override with {HERMETIC_SUBPROCESS_BUDGET_ENV})\n\
         \x20 stdout:    {} bytes seen, {} retained, truncated={}\n\
         \x20 stderr:    {} bytes seen, {} retained, truncated={}\n\
         \x20 verdict:   {}\n\
         \x20 last observed process tree:\n{snapshot}\n\
         --- stdout ---\n{}\n--- stderr ---\n{}",
        budget.as_secs_f64(),
        supervised.termination,
        elapsed.as_secs_f64(),
        budget.as_secs_f64(),
        output.capture.stdout.bytes_seen,
        output.capture.stdout.bytes_retained,
        output.capture.stdout.truncated,
        output.capture.stderr.bytes_seen,
        output.capture.stderr.bytes_retained,
        output.capture.stderr.truncated,
        if produced_output {
            "the child DID produce output before the kill, so it reached its own logic and blocked (or exited) afterwards"
        } else {
            "the child produced NO output at all, so it blocked before reaching any code that writes to its own streams"
        },
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn hermetic_subprocess_budget() -> Duration {
    std::env::var(HERMETIC_SUBPROCESS_BUDGET_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_HERMETIC_SUBPROCESS_BUDGET)
}

fn rendered_argv(command: &Command) -> String {
    let mut parts = vec![command.get_program().to_string_lossy().into_owned()];
    parts.extend(
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned()),
    );
    crate::engine::shell::quote_args(&parts)
}

/// Report the root process and its direct children as seen by the kernel.
///
/// Deliberately shallow and read-only: state plus wait channel is enough to
/// separate "blocked on I/O", "spinning", and "already a zombie whose pipes a
/// descendant still holds" without building a process-forensics framework.
#[cfg(target_os = "linux")]
fn process_tree_snapshot(root: u32) -> String {
    let Ok(entries) = fs::read_dir("/proc") else {
        return "    (/proc unavailable)".to_string();
    };
    let mut lines = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // The `comm` field is parenthesized and may contain spaces, so parse
        // the fixed fields from after its closing parenthesis.
        let Some((command, rest)) = stat
            .find('(')
            .and_then(|open| stat.rfind(')').map(|close| (open, close)))
            .filter(|(open, close)| open < close)
            .map(|(open, close)| (&stat[open + 1..close], &stat[close + 1..]))
        else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next().unwrap_or("?");
        let parent = fields
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        if pid != root && parent != root {
            continue;
        }
        let wchan = fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
        let wchan = wchan.trim();
        lines.push(format!(
            "    pid {pid} ppid {parent} state {state} comm {command} wchan {}",
            if wchan.is_empty() { "-" } else { wchan }
        ));
    }
    if lines.is_empty() {
        "    (no live /proc entry for the child or its children)".to_string()
    } else {
        lines.sort();
        lines.join("\n")
    }
}

#[cfg(not(target_os = "linux"))]
fn process_tree_snapshot(_root: u32) -> String {
    "    (process-tree snapshot is only collected on Linux)".to_string()
}

/// Isolates one test's Homeboy state: config, data, artifact, runtime, and
/// invocation roots.
///
/// # Concurrency contract
///
/// This guard repoints `HOME` for the whole process, which is not thread-safe:
/// `getenv`/`setenv` race, so a reader landing mid-write can observe `HOME` as
/// *absent* and fail with "HOME environment variable not set on Unix-like
/// system" on a host where it is plainly set.
///
/// `home_lock()` is held for this guard's entire lifetime, but that only
/// serializes **writers**. Readers never take it — including worker threads a
/// test spawns inside itself, which is why running the suite with
/// `--test-threads=1` did not stop the failures: serializing test *functions*
/// does nothing for threads *within* a test.
///
/// The hot resolvers therefore do not read the environment at all. `new()`
/// registers a process-local override via `paths::set_home_root_override`, and
/// `paths::homeboy()`, `paths::homeboy_data()`, and the invocation runtime root
/// read it under a `Mutex` so a concurrent repoint is ordered rather than torn
/// (#7505, #11266). `set_var("HOME", ..)` is retained alongside it for readers
/// that still consult `HOME` directly and for subprocesses.
///
/// `Drop` clears the override **before** restoring the environment, so no
/// window exists where the override still names a tempdir it is about to
/// delete.
pub struct HomeGuard {
    prior: Option<String>,
    prior_xdg_config_home: Option<String>,
    prior_xdg_cache_home: Option<String>,
    prior_xdg_data_home: Option<String>,
    prior_xdg_state_home: Option<String>,
    prior_xdg_runtime_dir: Option<String>,
    prior_data_dir: Option<String>,
    prior_daemon_state_dir: Option<String>,
    prior_daemon_namespace: Option<String>,
    prior_keep_daemon_in_process_group: Option<String>,
    prior_artifact_root: Option<String>,
    prior_runtime_tmpdir: Option<String>,
    prior_invocation_runtime: Option<String>,
    prior_no_update_check: Option<String>,
    prior_daemon_binary_sha: Option<String>,
    prior_controller_runtime_executable: Option<String>,
    prior_controller_runtime_source: Option<String>,
    prior_controller_runtime_identity: Option<String>,
    context: HermeticTestContext,
    _guard: Option<MutexGuard<'static, ()>>,
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

impl Default for AuditGuard {
    fn default() -> Self {
        Self::new()
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

impl Default for AuditHomeGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl HomeGuard {
    pub fn new() -> Self {
        Self::with_controller_runtime(TestBinary::CurrentTest)
    }

    /// Isolate in-process Homeboy state while selecting the controller runtime
    /// fixture that the test will pin or re-exec.
    ///
    /// Integration tests that exercise controller subprocesses must select
    /// [`TestBinary::HomeboyFixture`]. Their libtest executable is not the CLI
    /// and therefore cannot satisfy the production `--version` identity check.
    pub fn with_controller_runtime(binary: TestBinary) -> Self {
        let guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
        Self::new_with_guard(binary, Some(guard))
    }

    fn new_with_guard(binary: TestBinary, guard: Option<MutexGuard<'static, ()>>) -> Self {
        reset_cached_test_state();
        let prior = std::env::var("HOME").ok();
        let prior_xdg_config_home = std::env::var("XDG_CONFIG_HOME").ok();
        let prior_xdg_cache_home = std::env::var("XDG_CACHE_HOME").ok();
        let prior_xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
        let prior_xdg_state_home = std::env::var("XDG_STATE_HOME").ok();
        let prior_xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
        let prior_data_dir = std::env::var(crate::paths::HOMEBOY_DATA_DIR_ENV).ok();
        let prior_daemon_state_dir = std::env::var(crate::paths::DAEMON_STATE_DIR_ENV).ok();
        let prior_daemon_namespace = std::env::var(TEST_DAEMON_NAMESPACE_ENV).ok();
        let prior_keep_daemon_in_process_group =
            std::env::var(TEST_KEEP_DAEMON_IN_PROCESS_GROUP_ENV).ok();
        let prior_artifact_root = std::env::var("HOMEBOY_ARTIFACT_ROOT").ok();
        let prior_runtime_tmpdir = std::env::var("HOMEBOY_RUNTIME_TMPDIR").ok();
        let prior_invocation_runtime =
            std::env::var(crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV).ok();
        let prior_no_update_check = std::env::var("HOMEBOY_NO_UPDATE_CHECK").ok();
        let prior_daemon_binary_sha =
            std::env::var(crate::daemon::DAEMON_BINARY_SHA_OVERRIDE_ENV).ok();
        let prior_controller_runtime_executable =
            std::env::var(crate::controller_runtime::TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV).ok();
        let prior_controller_runtime_source =
            std::env::var(crate::controller_runtime::TEST_CONTROLLER_RUNTIME_SOURCE_ENV).ok();
        let prior_controller_runtime_identity =
            std::env::var(crate::controller_runtime::TEST_CONTROLLER_RUNTIME_IDENTITY_ENV).ok();
        // The isolated HOME hosts `~/.config/homeboy/extensions/**/*.sh`
        // capability scripts that tests execute. On `noexec`-`/tmp` hosts a
        // plain `TempDir::new()` lands the whole HOME on a `noexec` mount,
        // failing every capability-script test with exit 126 (#6760). Anchor
        // it (and the runtime tmpdir, which also hosts executables) on an
        // exec-capable root.
        let context = HermeticTestContext::new();
        std::env::set_var("HOME", context.home());
        // `set_var` above stays for the readers that still consult `HOME`
        // directly and for anything reading it after this process forks. The
        // override is what makes the *hot* resolvers — `paths::homeboy()`,
        // `paths::homeboy_data()`, and the invocation runtime root — race-free:
        // they read it under a lock instead of racing this `setenv` from
        // worker threads a test spawns inside itself (#7505).
        crate::paths::set_home_root_override(Some(context.home().to_path_buf()));
        // Preserve the legacy in-process data fallback while the subprocess
        // context uses explicit paths. Unit tests assert this resolver's XDG
        // behavior, so an inherited explicit data root must not override it.
        std::env::set_var("XDG_CONFIG_HOME", context.home().join(".config"));
        std::env::set_var("XDG_CACHE_HOME", context.home().join(".cache"));
        std::env::set_var("XDG_DATA_HOME", context.home().join(".local").join("share"));
        std::env::set_var(
            "XDG_STATE_HOME",
            context.home().join(".local").join("state"),
        );
        std::env::set_var("XDG_RUNTIME_DIR", context.runtime_dir());
        std::env::remove_var(crate::paths::HOMEBOY_DATA_DIR_ENV);
        std::env::set_var(crate::paths::DAEMON_STATE_DIR_ENV, context.daemon_dir());
        std::env::set_var(TEST_DAEMON_NAMESPACE_ENV, context.daemon_dir());
        std::env::set_var(TEST_KEEP_DAEMON_IN_PROCESS_GROUP_ENV, "1");
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
        // Name the fixture and its source, but do not copy anything: the copy
        // is a multi-hundred-megabyte binary and almost no test on this path
        // ever reads the contract. `ensure_test_controller_fixture` produces
        // the bytes at the few call sites that do.
        std::env::set_var(
            crate::controller_runtime::TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV,
            test_controller_fixture_path(binary),
        );
        std::env::set_var(
            crate::controller_runtime::TEST_CONTROLLER_RUNTIME_SOURCE_ENV,
            test_controller_fixture_source(binary),
        );
        std::env::set_var(
            crate::controller_runtime::TEST_CONTROLLER_RUNTIME_IDENTITY_ENV,
            crate::build_identity::current().display,
        );
        Self {
            prior,
            prior_xdg_config_home,
            prior_xdg_cache_home,
            prior_xdg_data_home,
            prior_xdg_state_home,
            prior_xdg_runtime_dir,
            prior_data_dir,
            prior_daemon_state_dir,
            prior_daemon_namespace,
            prior_keep_daemon_in_process_group,
            prior_artifact_root,
            prior_runtime_tmpdir,
            prior_invocation_runtime,
            prior_no_update_check,
            prior_daemon_binary_sha,
            prior_controller_runtime_executable,
            prior_controller_runtime_source,
            prior_controller_runtime_identity,
            context,
            _guard: guard,
        }
    }
}

impl Default for HomeGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Return a short-path tempdir suitable for the invocation runtime root.
///
/// The generated root must leave room for the short invocation-id leaf and the
/// production `sockaddr_un` headroom contract. Unlike the general runtime
/// tempdir, this root only holds invocation state and sockets, so it need not
/// be executable. Failing closed here prevents nested fixture Homeboy processes
/// from inheriting an override that production correctly rejects (#11867).
fn short_invocation_tempdir() -> TempDir {
    #[cfg(unix)]
    {
        sweep_leaked_test_tempdirs_once();
        let cache = SHORT_EXEC_CAPABLE_TEMP_BASE.get_or_init(|| Mutex::new(None));
        let cached = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(base) = cached.filter(|base| invocation_temp_base_fits(base)) {
            if let Ok(directory) = tempfile::Builder::new()
                .prefix(&owned_tempdir_prefix())
                .tempdir_in(&base)
            {
                if invocation_runtime_dir_fits(directory.path()) {
                    return directory;
                }
            }
        }

        for base in short_tempdir_candidates() {
            let Ok(directory) = tempfile::Builder::new()
                .prefix(&owned_tempdir_prefix())
                .tempdir_in(&base)
            else {
                continue;
            };
            if invocation_runtime_dir_fits(directory.path()) {
                *cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(base);
                return directory;
            }
        }

        panic!("no writable test invocation runtime root satisfies the sockaddr_un budget");
    }
    #[cfg(not(unix))]
    {
        marked_tempdir("invocation runtime tempdir")
    }
}

/// A tempdir carrying the owned marker prefix, for the platforms with no
/// exec-probe path. Every isolated home must be recognizable by name, on every
/// platform, or the reaper cannot tell it apart from someone else's scratch.
#[cfg(not(unix))]
fn marked_tempdir(context: &str) -> TempDir {
    tempfile::Builder::new()
        .prefix(&crate::cleanup::leaked_test_homes::owned_test_tempdir_prefix())
        .tempdir()
        .unwrap_or_else(|error| panic!("{context}: {error}"))
}

/// Ordered short base directories to consider for the invocation runtime root.
///
/// Conventional system roots are stable, short candidates. `$TMPDIR` is
/// intentionally excluded: on macOS it is commonly a long per-user path and
/// would make hermetic child behavior depend on the operator environment.
#[cfg(unix)]
fn short_tempdir_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut push_if_usable = |path: PathBuf| {
        if path.is_dir() && invocation_temp_base_fits(&path) && !candidates.contains(&path) {
            candidates.push(path);
        }
    };

    push_if_usable(PathBuf::from("/tmp"));
    push_if_usable(PathBuf::from("/var/tmp"));
    push_if_usable(PathBuf::from("/dev/shm"));
    candidates
}

/// Verify the actual generated root plus the 10-byte invocation state leaf.
/// Checking the root alone misses the path component every workload receives.
#[cfg(unix)]
fn invocation_runtime_dir_fits(root: &Path) -> bool {
    crate::engine::invocation::enforce_path_budget(&root.join(TEST_INVOCATION_ID)).is_ok()
}

/// Verify a candidate base can contain the PID-owned `tempfile` directory and
/// the state leaf that production appends beneath the allocated runtime root.
#[cfg(unix)]
fn invocation_temp_base_fits(base: &Path) -> bool {
    let allocated_root = base.join(format!(
        "{}{}",
        owned_tempdir_prefix(),
        "x".repeat(TEMPFILE_RANDOM_SUFFIX_BYTES)
    ));
    invocation_runtime_dir_fits(&allocated_root)
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

// The marker contract lives in production code, not here.
//
// A leaked tempdir outlives the process that made it, so the only code that can
// reclaim it is code with no test fixtures compiled in — the
// `leaked-test-homes` cleanup category. Two spellings of the same prefix would
// be two chances for the creator and the reaper to look at different names,
// which is the failure #11073 is made of. There is one spelling.
#[cfg(unix)]
use crate::cleanup::leaked_test_homes::{
    owned_test_tempdir_prefix as owned_tempdir_prefix, test_tempdir_owner_pid as tempdir_owner_pid,
    TEST_TEMPDIR_PREFIX,
};

/// Age past which a leaked tempdir with *no owner PID in its name* is
/// considered abandoned.
///
/// This is now only the fallback for directories written by a binary that
/// predates [`owned_tempdir_prefix`]. Current directories carry their creator's
/// PID and are reclaimed on liveness instead, without waiting out a timer.
#[cfg(unix)]
const LEAKED_TEMPDIR_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Sweep `hb-test-*` tempdirs abandoned by dead test processes.
///
/// `TempDir`'s RAII cleanup is correct for graceful exits but **cannot** run
/// when a process is `SIGKILL`ed — which on hardened / RAM-constrained hosts is
/// routine (OOM killer, watchdog restarts, hard test timeouts). Each killed
/// test leaks up to three `hb-test-*` directories; over many runs these fill
/// the disk (observed: 130 dirs / ~18G taking a host to 100%, see #9173).
///
/// Reclaim is driven by **owner liveness**, not age. Each directory carries the
/// PID that created it (see [`owned_tempdir_prefix`]), so the sweep can ask
/// whether that process still exists rather than waiting out a timer. This
/// matters because the age heuristic alone could not keep up: a leaked
/// directory holds a full copy of the test binary (~431M via `publish_pin`),
/// and at a normal test cadence an hour of accumulation reaches ~16G (#11353).
///
/// Checking liveness is also a *stronger* guarantee than the timer it replaces
/// — a running test's directory is protected because its process is alive, not
/// because it happens to be recent. PID reuse fails safe: a recycled PID makes
/// a dead directory look alive, so it simply survives to a later pass.
///
/// This is a best-effort safety net, gated to run **once per process** before
/// the first tempdir is created. It only removes directories:
/// - directly under a known tempdir root (never recurses into subdirs),
/// - whose name starts with `hb-test-`,
/// - that does not contain this process's active `TMPDIR`,
/// - that this process does not own, and whose owning PID is gone — or, for
///   names with no PID (written by an older binary), whose mtime is older than
///   [`LEAKED_TEMPDIR_MAX_AGE`].
///
/// All errors are swallowed — a failed sweep must never break a test.
#[cfg(unix)]
fn sweep_leaked_test_tempdirs(roots: &[PathBuf], active_tempdir: Option<&Path>) {
    let now = std::time::SystemTime::now();
    let active_tempdir =
        active_tempdir.map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
    let mut swept_roots: Vec<PathBuf> = Vec::new();
    for root in roots {
        if swept_roots.contains(root) {
            continue;
        }
        swept_roots.push(root.clone());
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with(TEST_TEMPDIR_PREFIX) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_dir() {
                continue;
            }
            let canonical_path = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if active_tempdir
                .as_ref()
                .is_some_and(|active| active.starts_with(&canonical_path))
            {
                continue;
            }
            let abandoned = match tempdir_owner_pid(name) {
                // Never reclaim our own directories: this process is mid-run
                // and still owns them.
                Some(pid) if pid == std::process::id() => false,
                Some(pid) => !crate::process::pid_is_running(pid),
                // No owner recorded — written by a binary predating the PID
                // prefix. Fall back to the age heuristic.
                None => metadata
                    .modified()
                    .ok()
                    .and_then(|modified| now.duration_since(modified).ok())
                    .map(|age| age >= LEAKED_TEMPDIR_MAX_AGE)
                    .unwrap_or(false),
            };
            if abandoned {
                let _ = fs::remove_dir_all(&path);
            }
        }
    }
}

/// Run [`sweep_leaked_test_tempdirs`] at most once per process, across every
/// candidate tempdir root, before the first tempdir of the run is created.
#[cfg(unix)]
fn sweep_leaked_test_tempdirs_once() {
    LEAKED_TEMPDIR_SWEEP.get_or_init(|| {
        let mut roots = exec_capable_tempdir_candidates();
        for extra in short_tempdir_candidates() {
            if !roots.contains(&extra) {
                roots.push(extra);
            }
        }
        let active_tempdir = std::env::var_os("TMPDIR").map(PathBuf::from);
        sweep_leaked_test_tempdirs(&roots, active_tempdir.as_deref());
    });
}

/// Reuse a validated base directory, but always create a new child tempdir.
/// If a cached base disappears or becomes unavailable, retry the normal ordered
/// probe and replace the cached base only after a successful execution probe.
#[cfg(unix)]
fn tempdir_with_cached_exec_base(
    cache: &Mutex<Option<PathBuf>>,
    candidates: Vec<PathBuf>,
    prefix: &str,
    probe: impl Fn(&Path) -> bool,
) -> TempDir {
    // Best-effort reclaim of tempdirs leaked by killed test processes (#9173).
    // Runs once per process before the first tempdir is created.
    sweep_leaked_test_tempdirs_once();

    let cached = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Some(base) = cached {
        if let Ok(directory) = tempfile::Builder::new().prefix(prefix).tempdir_in(&base) {
            return directory;
        }
    }

    for base in candidates {
        let Ok(directory) = tempfile::Builder::new().prefix(prefix).tempdir_in(&base) else {
            continue;
        };
        if probe(directory.path()) {
            *cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(base);
            return directory;
        }
        // Not exec-capable (e.g. `noexec` mount) — drop it and try the next
        // candidate rather than handing back a dir scripts cannot run from.
    }
    // The fallback keeps the owned prefix. It used to be a bare `TempDir::new`,
    // which produced an *unmarked* `.tmpXXXXXX` directory — invisible to the
    // in-process sweep below and to the `leaked-test-homes` cleanup category,
    // because both identify a home by its name. On any host where no candidate
    // root passes the exec probe, that is every isolated home in the run, and it
    // leaks with nothing able to recognize it (#11073).
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("exec-capable tempdir fallback")
}

#[cfg(unix)]
fn exec_capable_tempdir_candidates() -> Vec<PathBuf> {
    let mut roots = Vec::new();
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
    roots
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
        tempdir_with_cached_exec_base(
            EXEC_CAPABLE_TEMP_BASE.get_or_init(|| Mutex::new(None)),
            exec_capable_tempdir_candidates(),
            &owned_tempdir_prefix(),
            dir_allows_exec,
        )
    }
    #[cfg(not(unix))]
    {
        marked_tempdir("exec-capable tempdir")
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // Clear the override before restoring `HOME` so no window exists where
        // the override still points at this guard's tempdir — which `Drop` is
        // about to delete — while the environment already names the next root.
        crate::paths::set_home_root_override(None);
        match &self.prior {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match &self.prior_xdg_config_home {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match &self.prior_xdg_cache_home {
            Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        match &self.prior_xdg_data_home {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        match &self.prior_xdg_state_home {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        match &self.prior_xdg_runtime_dir {
            Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        match &self.prior_data_dir {
            Some(value) => std::env::set_var(crate::paths::HOMEBOY_DATA_DIR_ENV, value),
            None => std::env::remove_var(crate::paths::HOMEBOY_DATA_DIR_ENV),
        }
        match &self.prior_daemon_state_dir {
            Some(value) => std::env::set_var(crate::paths::DAEMON_STATE_DIR_ENV, value),
            None => std::env::remove_var(crate::paths::DAEMON_STATE_DIR_ENV),
        }
        match &self.prior_daemon_namespace {
            Some(value) => std::env::set_var(TEST_DAEMON_NAMESPACE_ENV, value),
            None => std::env::remove_var(TEST_DAEMON_NAMESPACE_ENV),
        }
        match &self.prior_keep_daemon_in_process_group {
            Some(value) => std::env::set_var(TEST_KEEP_DAEMON_IN_PROCESS_GROUP_ENV, value),
            None => std::env::remove_var(TEST_KEEP_DAEMON_IN_PROCESS_GROUP_ENV),
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
        match &self.prior_controller_runtime_executable {
            Some(value) => std::env::set_var(
                crate::controller_runtime::TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV,
                value,
            ),
            None => std::env::remove_var(
                crate::controller_runtime::TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV,
            ),
        }
        match &self.prior_controller_runtime_source {
            Some(value) => std::env::set_var(
                crate::controller_runtime::TEST_CONTROLLER_RUNTIME_SOURCE_ENV,
                value,
            ),
            None => {
                std::env::remove_var(crate::controller_runtime::TEST_CONTROLLER_RUNTIME_SOURCE_ENV)
            }
        }
        match &self.prior_controller_runtime_identity {
            Some(value) => std::env::set_var(
                crate::controller_runtime::TEST_CONTROLLER_RUNTIME_IDENTITY_ENV,
                value,
            ),
            None => std::env::remove_var(
                crate::controller_runtime::TEST_CONTROLLER_RUNTIME_IDENTITY_ENV,
            ),
        }
        reset_cached_test_state();
    }
}

/// The executable a hermetic test command runs, and the source a controller
/// fixture for that selection is copied from.
fn test_binary_path(binary: TestBinary) -> PathBuf {
    match binary {
        TestBinary::CurrentTest => std::env::current_exe().expect("current test executable"),
        TestBinary::HomeboyFixture => PathBuf::from(
            std::env::var_os("CARGO_BIN_EXE_homeboy")
                .expect("CARGO_BIN_EXE_homeboy fixture binary"),
        ),
    }
}

/// The source for a controller-runtime fixture. A unit-test executable is a
/// libtest harness, not the Homeboy CLI: invoking it with `--version` reports
/// its filtered test count. Give in-process tests a deterministic version
/// responder instead; integration tests that need a real re-exec select
/// `HomeboyFixture` explicitly.
fn test_controller_fixture_source(binary: TestBinary) -> PathBuf {
    match binary {
        TestBinary::CurrentTest => {
            #[cfg(unix)]
            {
                SHARED_CONTROLLER_RUNTIME_VERSION_FIXTURE
                    .get_or_init(|| {
                        let root = SHARED_CONTROLLER_RUNTIME_FIXTURE.get_or_init(exec_capable_tempdir);
                        let source = root.path().join("homeboy-controller-version-fixture");
                        let identity = crate::build_identity::current().display.clone();
                        let identity_json = serde_json::json!({
                            "data": { "display": identity }
                        });
                        fs::write(
                            &source,
                            format!(
                                "#!/bin/sh\nif [ \"${{1:-}}\" = --version ]; then\n  printf '%s\\n' '{identity}'\n  exit 0\nfi\nif [ \"${{1:-}}\" = self ] && [ \"${{2:-}}\" = identity ]; then\n  printf '%s\\n' '{identity_json}'\n  exit 0\nfi\nexit 64\n"
                            ),
                        )
                        .expect("write controller version fixture");
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&source, fs::Permissions::from_mode(0o500))
                            .expect("seal controller version fixture");
                        source
                    })
                    .clone()
            }
            #[cfg(not(unix))]
            {
                test_binary_path(binary)
            }
        }
        TestBinary::HomeboyFixture => test_binary_path(binary),
    }
}

/// Where this process publishes its controller-runtime fixture for `binary`.
///
/// Allocating the path is cheap — one tempdir per process, shared by every
/// isolated home. The expensive part, copying the source executable, is
/// deferred to [`ensure_test_controller_fixture`].
fn test_controller_fixture_path(binary: TestBinary) -> PathBuf {
    let fixture = match binary {
        TestBinary::CurrentTest => &SHARED_CONTROLLER_RUNTIME_FIXTURE,
        TestBinary::HomeboyFixture => &SHARED_HOMEBOY_CONTROLLER_RUNTIME_FIXTURE,
    };
    fixture
        .get_or_init(exec_capable_tempdir)
        .path()
        .join(CONTROLLER_FIXTURE_FILE_NAME)
}

/// Materialize the controller-runtime fixture named by `path`, once.
///
/// The fixture is a byte-identical copy of the running test binary, which is
/// ~700 MB unoptimized (see the `profile.dev.package.sha2` note in the
/// workspace `Cargo.toml`). It used to be copied eagerly from
/// [`HomeGuard::new`], i.e. once per *process*. Under `cargo test` that is once
/// per test binary — already paid by every binary whose tests never touch the
/// contract. Under nextest, which runs one process per test, it is once per
/// **test**: all ~2,600 `with_isolated_home` call sites paying for the handful
/// that read `TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV`. Those readers call this
/// instead, so the copy happens only where the bytes are actually used.
///
/// Deliberately defensive, because it can be handed any path a test put in the
/// contract. It writes only when *all* of the following hold:
/// - nothing exists at `path` yet,
/// - `path` carries the fixture's own file name,
/// - `path` is exactly what the contract currently names, and
/// - a source executable is recorded alongside it.
///
/// A test that points the contract at a binary of its own therefore keeps
/// precisely the bytes it chose.
pub(crate) fn ensure_test_controller_fixture(path: &Path) {
    // The branch every call after the first takes, in this process or in the
    // parent that handed the path down.
    if path.exists() {
        return;
    }
    if path.file_name().and_then(|name| name.to_str()) != Some(CONTROLLER_FIXTURE_FILE_NAME) {
        return;
    }
    let Some(destination) =
        std::env::var_os(crate::controller_runtime::TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV)
    else {
        return;
    };
    if Path::new(&destination) != path {
        return;
    }
    let Some(source) =
        std::env::var_os(crate::controller_runtime::TEST_CONTROLLER_RUNTIME_SOURCE_ENV)
    else {
        return;
    };
    let source = PathBuf::from(source);

    let mut published = PUBLISHED_CONTROLLER_FIXTURES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Re-check under the lock. A sibling thread may have published while we
    // waited, and paying the copy twice is the one thing this exists to avoid.
    if published.contains(path) || path.exists() {
        return;
    }
    publish_test_controller_fixture(&source, path);
    published.insert(path.to_path_buf());
}

/// Copy `source` into place at `destination` so that `destination` never
/// appears in a partial state.
///
/// The copy lands on a per-process staging name and is linked into place, which
/// fails cleanly if someone else got there first. That matters because one
/// destination can now be shared by more than one process: a child test process
/// materializes into the tempdir its parent named, and a plain `fs::copy`
/// straight to the destination would let one observe the other's half-written
/// file. The two racers copy the same source bytes, so either winner is correct.
///
/// The link is deliberately *staging to destination* and never *source to
/// destination*: the fixture is sealed read-only, and a hard link to the source
/// would seal the real test binary along with it.
fn publish_test_controller_fixture(source: &Path, destination: &Path) {
    let parent = destination
        .parent()
        .expect("controller fixture destination has a parent directory");
    fs::create_dir_all(parent).expect("create controller fixture directory");
    let staging = parent.join(format!(
        "{CONTROLLER_FIXTURE_FILE_NAME}.staging.{}",
        std::process::id()
    ));
    let _ = fs::remove_file(&staging);
    fs::copy(source, &staging).expect("copy controller fixture");
    make_test_controller_fixture_read_only(&staging);
    match fs::hard_link(&staging, destination) {
        Ok(()) => {}
        // Another process published the same source bytes first.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!(
            "publish controller fixture at {}: {error}",
            destination.display()
        ),
    }
    let _ = fs::remove_file(&staging);
}

fn make_test_controller_fixture_read_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o500))
            .expect("seal controller fixture");
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)
            .expect("inspect controller fixture")
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).expect("seal controller fixture");
    }
}

/// Source executable selected by the hermetic controller-runtime test contract.
///
/// Materializes the fixture, so callers may execute, hash, or stat the returned
/// path exactly as they could when the copy was made eagerly.
pub fn controller_runtime_test_executable() -> PathBuf {
    test_controller_fixture_source(TestBinary::CurrentTest)
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
    crate::observation::runs_service::runner_evidence::reset_runner_evidence_provider_for_test();
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

pub fn write_component_registration(home: &Path, id: &str, local_path: &Path) {
    let dir = home.join(".config/homeboy/components");
    fs::create_dir_all(&dir).expect("components dir");
    fs::write(
        dir.join(format!("{id}.json")),
        serde_json::json!({
            "local_path": local_path,
            "remote_path": format!("wp-content/plugins/{id}")
        })
        .to_string(),
    )
    .expect("component registration");
}

pub fn write_extension_fixture(root: &Path, id: &str) {
    write_extension_fixture_with_version(root, id, "1.0.0");
}

pub fn write_extension_fixture_with_version(root: &Path, id: &str, version: &str) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).expect("extension dir");
    fs::write(
        dir.join(format!("{}.json", id)),
        format!(
            r#"{{
  "name": "{} extension",
  "version": "{}"
}}"#,
            id, version
        ),
    )
    .expect("extension manifest");
}

/// Git command execution shared by test fixtures.
///
/// Topology-specific setup stays at the caller: branch names, remotes, commit
/// identity, and failure scenarios remain visible in each test.
pub struct GitFixture<'a> {
    repo: &'a Path,
    identity: Option<(&'static str, &'static str)>,
}

impl<'a> GitFixture<'a> {
    pub fn new(repo: &'a Path) -> Self {
        Self {
            repo,
            identity: None,
        }
    }

    fn with_identity(repo: &'a Path, name: &'static str, email: &'static str) -> Self {
        Self {
            repo,
            identity: Some((name, email)),
        }
    }

    pub fn execute(&self, args: &[&str]) -> Output {
        let mut command = Command::new("git");
        command.args(args).current_dir(self.repo);
        if let Some((name, email)) = self.identity {
            command
                .env("GIT_AUTHOR_NAME", name)
                .env("GIT_AUTHOR_EMAIL", email)
                .env("GIT_COMMITTER_NAME", name)
                .env("GIT_COMMITTER_EMAIL", email);
        }
        command.output().expect("git fixture command")
    }
}

pub fn shared_git_repo_fixture(name: &str) -> (TempDir, PathBuf) {
    git_repo_fixture(name, false)
}

pub fn shared_committed_git_repo_fixture(name: &str) -> (TempDir, PathBuf) {
    git_repo_fixture(name, true)
}

fn git_repo_fixture(name: &str, committed: bool) -> (TempDir, PathBuf) {
    let temp = TempDir::new().expect("git fixture tempdir");
    let repo = temp.path().join(name);
    std::fs::create_dir_all(&repo).expect("git fixture repo");
    run_git_template_command(&repo, &["init", "-q", "-b", "main"]);
    if committed {
        std::fs::write(repo.join("README.md"), "# homeboy test fixture\n")
            .expect("git fixture readme");
        run_git_template_command(&repo, &["add", "README.md"]);
        run_git_template_command(&repo, &["commit", "-q", "-m", "test fixture"]);
    }
    (temp, repo)
}

pub fn run_git_fixture_command(repo: &Path, args: &[&str]) {
    let output = GitFixture::with_identity(repo, "homeboy-test", "homeboy-test@example.invalid")
        .execute(args);
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
    let output = GitFixture::with_identity(repo, "homeboy-test", "homeboy-test@example.invalid")
        .execute(args);
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

/// The libtest name of a test in `module_path`, for passing to `--exact`.
///
/// Several tests re-invoke their own binary to get a second process -- a lock
/// holder, a lease claimant -- by naming the child test with `--exact`. Those
/// names were written as string literals, and the crate extraction moved every
/// module underneath them. A stale name does not error: libtest matches zero
/// tests, prints `running 0 tests`, and exits 0. So the child appears to
/// succeed while never running, and `assert!(status.success())` passes having
/// proved nothing (#12373-adjacent; found via the one case that did fail,
/// `daemon_operation_lock_recovers_after_owner_exits_without_drop`, whose parent
/// waits on a file the child never writes).
///
/// Deriving the name removes the literal. libtest strips the crate segment from
/// `module_path!()`, so `homeboy_core::daemon::daemon_test` is addressed as
/// `daemon::daemon_test`.
pub fn harness_test_name(module_path: &str, test: &str) -> String {
    let module = module_path
        .split_once("::")
        .map(|(_crate_name, rest)| rest)
        .unwrap_or(module_path);
    format!("{module}::{test}")
}

/// Run one `#[ignore]`d test in this same binary as a child process, and prove
/// it actually ran.
///
/// `status.success()` alone is not proof: a name that matches nothing also
/// exits 0. The child's own summary line is the measurement.
pub fn run_child_test(
    command: &mut std::process::Command,
    test_name: &str,
) -> std::process::Output {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("run child test {test_name}: {error}"));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("running 0 tests"),
        "child test `{test_name}` matched nothing, so the assertion on its exit status \
         would have passed without executing anything:\n{combined}"
    );
    output
}

/// Serializes tests that mutate or capture process-global environment state.
pub fn env_lock() -> MutexGuard<'static, ()> {
    home_lock().lock().unwrap_or_else(|e| e.into_inner())
}

/// Restores one process-global environment value when dropped.
///
/// Callers must hold [`env_lock`] while this guard is live. [`HomeGuard`]
/// already holds that lock, so it composes with [`with_isolated_home`].
pub struct EnvVarGuard {
    name: String,
    prior: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    pub fn set(name: &str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prior = std::env::var_os(name);
        unsafe { std::env::set_var(name, value) };
        Self {
            name: name.to_string(),
            prior,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => unsafe { std::env::set_var(&self.name, value) },
            None => unsafe { std::env::remove_var(&self.name) },
        }
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

/// Spin up a single-shot localhost HTTP server returning `status` once, used to
/// probe public-artifact-URL reachability. Returns the base URL ending in
/// `/homeboy`. Shared by `runs` and `bench` artifact-viewer tests.
pub fn serve_public_artifact_base_once(status: u16) -> String {
    serve_public_artifact_base(status, 1)
}

/// Spin up a bounded localhost HTTP server returning `status` for each probe.
///
/// A publication manifest and its materialized children each validate their
/// public URLs, so tests must declare the number of expected probes rather than
/// accidentally treating the first response as a shared result.
pub fn serve_public_artifact_base(status: u16, request_count: usize) -> String {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    assert!(
        request_count > 0,
        "public artifact server needs a request budget"
    );
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind public artifact server");
    let addr = listener.local_addr().expect("server address");
    std::thread::spawn(move || {
        for _ in 0..request_count {
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
        }
    });
    format!("http://{addr}/homeboy")
}

/// A minimal in-process implementation of Homeboy's public reverse-broker HTTP
/// contract. It is intentionally owned by core test support so binary tests and
/// runner tests exercise the same persisted broker behavior.
pub struct ReverseBrokerFixture {
    pub store: JobStore,
    runner_id: String,
    broker_url: String,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// An authenticated reverse broker backed by the daemon's production HTTP
/// routes and durable job store. The Lab runner registers its staging provider
/// before using this fixture.
pub struct AuthenticatedReverseBrokerFixture {
    pub store: JobStore,
    runner_id: String,
    broker_url: String,
    token: String,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl AuthenticatedReverseBrokerFixture {
    pub fn start(runner_id: impl Into<String>) -> Self {
        let runner_id = runner_id.into();
        let mut auth = crate::broker_auth::BrokerAuthStore::default();
        let credential = auth
            .pair(
                format!("test-{runner_id}"),
                &runner_id,
                [
                    crate::broker_auth::BrokerScope::Submit,
                    crate::broker_auth::BrokerScope::Work,
                ]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            )
            .expect("pair authenticated reverse broker fixture");
        auth.save()
            .expect("persist authenticated reverse broker fixture auth");
        let token = credential.token;
        let store = JobStore::default();
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind authenticated reverse broker fixture");
        listener
            .set_nonblocking(true)
            .expect("make authenticated reverse broker fixture nonblocking");
        let broker_url = format!("http://{}", listener.local_addr().expect("broker address"));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_store = store.clone();
        let handle = std::thread::spawn(move || {
            while !thread_shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("make authenticated reverse broker fixture stream blocking");
                        crate::daemon::handle_reverse_broker_test_connection(stream, &thread_store)
                            .expect("serve authenticated reverse broker fixture request");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => {
                        panic!("accept authenticated reverse broker fixture request: {error}")
                    }
                }
            }
        });
        Self {
            store,
            runner_id,
            broker_url,
            token,
            shutdown,
            handle: Some(handle),
        }
    }

    pub fn url(&self) -> &str {
        &self.broker_url
    }

    pub fn runner_id(&self) -> &str {
        &self.runner_id
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn jobs(&self) -> Vec<Job> {
        self.store.list()
    }
}

impl Drop for AuthenticatedReverseBrokerFixture {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.broker_url.trim_start_matches("http://"));
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .expect("authenticated reverse broker fixture joins");
        }
    }
}

impl ReverseBrokerFixture {
    pub fn start(runner_id: impl Into<String>) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};

        let runner_id = runner_id.into();
        let store = JobStore::default();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind reverse broker fixture");
        listener
            .set_nonblocking(true)
            .expect("make reverse broker fixture nonblocking");
        let broker_url = format!("http://{}", listener.local_addr().expect("broker address"));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_store = store.clone();
        let thread_runner_id = runner_id.clone();
        let handle = std::thread::spawn(move || {
            while !thread_shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("make reverse broker fixture stream blocking");
                        let request = read_broker_request(&mut stream);
                        let response = handle_reverse_broker_request(
                            &thread_store,
                            &thread_runner_id,
                            request,
                        );
                        write_broker_response(&mut stream, response);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => panic!("accept reverse broker fixture request: {error}"),
                }
            }
        });
        Self {
            store,
            runner_id,
            broker_url,
            shutdown,
            handle: Some(handle),
        }
    }

    pub fn url(&self) -> &str {
        &self.broker_url
    }

    pub fn runner_id(&self) -> &str {
        &self.runner_id
    }

    pub fn enqueue(&self, request: RemoteRunnerJobRequest) -> Job {
        self.store
            .submit_runner_api_fixture(request)
            .expect("enqueue reverse broker fixture job")
    }

    pub fn jobs(&self) -> Vec<Job> {
        self.store.list()
    }
}

impl Drop for ReverseBrokerFixture {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;

        self.shutdown.store(true, Ordering::SeqCst);
        // Wake the nonblocking accept loop before joining it.
        let _ = TcpStream::connect(self.broker_url.trim_start_matches("http://"));
        if let Some(handle) = self.handle.take() {
            handle.join().expect("reverse broker fixture joins");
        }
    }
}

struct ReverseBrokerRequest {
    method: String,
    path: String,
    body: serde_json::Value,
}

fn read_broker_request(stream: &mut TcpStream) -> ReverseBrokerRequest {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut chunk).expect("read broker request");
        assert_ne!(read, 0, "broker request closed before headers");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let mut request_line = headers
        .lines()
        .next()
        .expect("broker request line")
        .split_whitespace();
    let method = request_line
        .next()
        .expect("broker request method")
        .to_string();
    let path = request_line
        .next()
        .expect("broker request path")
        .to_string();
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    let body_start = header_end + 4;
    while buffer.len() < body_start + content_length {
        let read = stream.read(&mut chunk).expect("read broker request body");
        assert_ne!(read, 0, "broker request closed before body");
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = if content_length == 0 {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&buffer[body_start..body_start + content_length])
            .expect("parse broker request JSON")
    };
    ReverseBrokerRequest { method, path, body }
}

fn handle_reverse_broker_request(
    store: &JobStore,
    runner_id: &str,
    request: ReverseBrokerRequest,
) -> serde_json::Value {
    use serde_json::json;

    let ok = |body| json!({ "success": true, "data": { "body": body } });
    if request.method == "GET" && request.path == "/jobs" {
        let active_runner_jobs = store.active_runner_jobs();
        let stale_runner_jobs = store.stale_runner_jobs();
        return ok(json!({
            "command": "api.jobs.list",
            "jobs": store.list(),
            "active_runner_job_count": active_runner_jobs.len(),
            "active_runner_jobs": active_runner_jobs,
            "stale_runner_job_count": stale_runner_jobs.len(),
            "stale_runner_jobs": stale_runner_jobs,
        }));
    }
    if request.method == "POST" && request.path == "/runner/sessions" {
        return ok(json!({ "registered": true }));
    }
    if request.method == "POST" && request.path == "/runner/jobs" {
        let typed_submission = serde_json::from_value::<
            homeboy_runner_contract::RunnerApiSubmitRequest,
        >(request.body.clone());
        let is_typed_submission = typed_submission.is_ok();
        let job = match typed_submission {
            Ok(submitted) => store
                .submit_runner_api_request(submitted)
                .expect("submit envelope broker job"),
            Err(_) => {
                let submitted: RemoteRunnerJobRequest = serde_json::from_value(request.body)
                    .expect("parse legacy reverse broker job submission");
                store
                    .submit_remote_runner_job(submitted)
                    .expect("submit legacy broker job")
            }
        };
        if is_typed_submission {
            return ok(json!({
                "response": homeboy_runner_contract::RunnerApiSubmitResponse {
                    schema: homeboy_runner_contract::RUNNER_API_SUBMIT_RESPONSE_SCHEMA.to_string(),
                    api_version: homeboy_runner_contract::RUNNER_API_V1,
                    outcome: homeboy_runner_contract::RunnerApiSubmitOutcome::Accepted {
                        job_id: job.id.to_string(),
                        job_status: serde_json::to_value(job.status)
                            .expect("serialize fixture job status")
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    },
                },
                "job": job,
            }));
        }
        return ok(json!({ "job": job }));
    }
    if request.method == "POST" && request.path == "/runner/jobs/submissions/lookup" {
        let submission_key = request.body["submission_key"]
            .as_str()
            .expect("fixture submission key");
        return ok(json!({ "result": store.lookup_remote_runner_submission(submission_key) }));
    }
    if request.method == "POST" && request.path == "/runner/jobs/claim" {
        let claim = store
            .claim_remote_runner_job(runner_id, None, 30_000, None)
            .expect("claim broker job");
        return ok(json!({ "claim": claim }));
    }
    if request.method == "POST" && request.path == "/runner/jobs/reconcile" {
        let reconciled = store
            .reconcile_expired_remote_runner_claims_for_runner(
                chrono::Utc::now().timestamp_millis().max(0) as u64,
                Some(runner_id),
            )
            .expect("reconcile broker jobs");
        return ok(json!({
            "reconciled_count": reconciled.len(),
            "reconciled": reconciled,
        }));
    }
    // Reverse-runner file transfer (`RunnerFileChannel::BrokerHttp`) posts
    // `/files/{mkdir,upload,download}` for the same host the fixture runs on, so
    // the fixture performs the real filesystem operation. Without these the
    // detached Lab cook's Homeboy-owned artifact-directory creation fails with
    // "unknown reverse broker fixture path" (#9408).
    if request.method == "POST" && request.path == "/files/mkdir" {
        let path = request.body["path"].as_str().expect("broker mkdir path");
        std::fs::create_dir_all(path).expect("broker fixture mkdir");
        return ok(json!({ "created": true }));
    }
    if request.method == "POST" && request.path == "/files/upload" {
        use base64::Engine;
        let path = request.body["path"].as_str().expect("broker upload path");
        let encoded = request.body["content_base64"]
            .as_str()
            .expect("broker upload content_base64");
        let content = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("decode broker upload");
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent).expect("broker fixture upload parent");
        }
        if request.body["private"].as_bool().unwrap_or(false) {
            use std::io::Write;
            let temporary = format!("{path}.fixture-private.tmp");
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temporary)
                .expect("broker fixture private create");
            file.write_all(&content)
                .expect("broker fixture private write");
            file.sync_all().expect("broker fixture private sync");
            std::fs::rename(&temporary, path).expect("broker fixture private publish");
        } else {
            std::fs::write(path, content).expect("broker fixture upload write");
        }
        return ok(json!({ "uploaded": true }));
    }
    if request.method == "POST" && request.path == "/files/upload-chunk" {
        use base64::Engine;
        use std::io::Write;
        let path = request.body["path"].as_str().expect("broker chunk path");
        let upload_id = request.body["upload_id"].as_str().expect("broker chunk id");
        let offset = request.body["offset"]
            .as_u64()
            .expect("broker chunk offset");
        let size = request.body["size_bytes"]
            .as_u64()
            .expect("broker chunk size");
        let content = base64::engine::general_purpose::STANDARD
            .decode(
                request.body["content_base64"]
                    .as_str()
                    .expect("broker chunk content"),
            )
            .expect("decode broker chunk");
        let temp = format!("{path}.{upload_id}.fixture-upload");
        let current = std::fs::metadata(&temp)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        assert_eq!(current, offset, "broker chunk offset");
        assert!(current + content.len() as u64 <= size, "broker chunk size");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&temp)
            .expect("open broker chunk");
        file.write_all(&content).expect("write broker chunk");
        if request.body["final"].as_bool().unwrap_or(false) {
            assert_eq!(current + content.len() as u64, size, "broker final size");
            std::fs::rename(&temp, path).expect("publish broker chunk");
        }
        return ok(json!({ "uploaded": true }));
    }
    if request.method == "POST" && request.path == "/files/download" {
        use base64::Engine;
        let path = request.body["path"].as_str().expect("broker download path");
        let content = std::fs::read(path).expect("broker fixture download read");
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        return ok(json!({ "content_base64": encoded }));
    }
    if request.method == "GET" && request.path == "/files/capabilities" {
        return ok(json!({
            "protocol_version": 1,
            "capabilities": ["private_file_chunk_upload"],
            "max_upload_bytes": 64 * 1024 * 1024,
            "max_chunk_bytes": 64 * 1024,
        }));
    }
    if request.method == "GET" && request.path.starts_with("/jobs/reconcile") {
        return ok(json!({ "reconciled": [] }));
    }
    if request.method == "GET" {
        if let Some(job_path) = request.path.strip_prefix("/jobs/") {
            let (job_id, action) = job_path.split_once('/').unwrap_or((job_path, ""));
            let execution_context_probe_runner = matches!(action, "" | "events")
                .then(|| job_id.strip_prefix("runner-exec:"))
                .flatten()
                .and_then(|runner| runner.strip_suffix(":reverse_broker"));
            let job = uuid::Uuid::parse_str(job_id)
                .ok()
                .and_then(|job_id| store.get(job_id).ok())
                .or_else(|| {
                    execution_context_probe_runner
                        .map(|probe_runner| {
                            // The direct-session cancellation probe identifies the
                            // planned execution, while the reverse broker owns UUID
                            // job ids. Resolve only that exact planned-runner shape.
                            store
                                .list()
                                .into_iter()
                                .find(|job| job.target_runner_id.as_deref() == Some(probe_runner))
                        })
                        .flatten()
                })
                .expect("broker job");
            return match action {
                "" => ok(json!({ "job": job })),
                "events" => ok(json!({
                    "events": store.events(job.id).expect("broker job events")
                })),
                _ => json!({
                    "success": false,
                    "error": { "message": "unknown reverse broker fixture read path" }
                }),
            };
        }
    }
    if let Some(job_id) = request.path.strip_prefix("/runner/jobs/") {
        let (job_id, action) = job_id.split_once('/').unwrap_or((job_id, ""));
        let job_id = uuid::Uuid::parse_str(job_id).expect("broker job id");
        if request.method == "GET" && action.is_empty() {
            return ok(json!({ "job": store.get(job_id).expect("broker job") }));
        }
        let claim_id = request.body["claim_id"].as_str().expect("broker claim id");
        let result = match action {
            "events" => store
                .append_remote_runner_event(
                    job_id,
                    runner_id,
                    claim_id,
                    JobEventKind::Progress,
                    request.body["message"].as_str().map(ToString::to_string),
                    request.body.get("data").cloned(),
                )
                .map(|event| json!({ "event": event })),
            "heartbeat" => store
                .renew_remote_runner_claim_with_workspace_owner_lease(
                    job_id, runner_id, claim_id, 30_000, None, None,
                )
                .map(|job| json!({ "job": job })),
            "consume" => store
                .consume_remote_runner_execution(
                    job_id,
                    runner_id,
                    claim_id,
                    request.body["context_id"]
                        .as_str()
                        .expect("broker execution context id"),
                )
                .map(|job| json!({ "job": job })),
            "finish" => store
                .finish_remote_runner_job(
                    job_id,
                    runner_id,
                    claim_id,
                    serde_json::from_value::<RemoteRunnerJobResult>(request.body["result"].clone())
                        .expect("parse broker finish result"),
                )
                .map(|job| json!({ "job": job })),
            _ => Err(crate::error::Error::internal_unexpected(
                "unknown reverse broker fixture path",
            )),
        };
        return match result {
            Ok(body) => ok(body),
            Err(error) => json!({ "success": false, "error": { "message": error.message } }),
        };
    }
    json!({ "success": false, "error": { "message": "unknown reverse broker fixture path" } })
}

fn write_broker_response(stream: &mut TcpStream, body: serde_json::Value) {
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    )
    .expect("write broker response");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Holds the one environment lock across sentinel setup, HomeGuard's
    /// snapshot, both restoration boundaries, and panic unwinding.
    struct HomeGuardTestScope {
        prior: Vec<(&'static str, Option<std::ffi::OsString>)>,
        home: Option<HomeGuard>,
        unwind_validation: Option<Arc<AtomicBool>>,
        _guard: MutexGuard<'static, ()>,
    }

    impl HomeGuardTestScope {
        fn new(names: &[&'static str], setup: impl FnOnce()) -> Self {
            let guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
            let mut scope = Self {
                prior: names
                    .iter()
                    .map(|name| (*name, std::env::var_os(name)))
                    .collect(),
                home: None,
                unwind_validation: None,
                _guard: guard,
            };
            setup();
            scope.home = Some(HomeGuard::new_with_guard(TestBinary::CurrentTest, None));
            scope
        }

        fn home(&self) -> &HomeGuard {
            self.home.as_ref().expect("isolated home is active")
        }

        fn restore_isolated_home(&mut self) {
            drop(self.home.take());
        }

        fn restore_real_environment_and_assert(&mut self) {
            self.restore_isolated_home();
            for (name, value) in &self.prior {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
            for (name, value) in &self.prior {
                assert_eq!(&std::env::var_os(name), value, "{name} was fully restored");
            }
        }

        fn record_unwind_validation(&mut self, validation: Arc<AtomicBool>) {
            self.unwind_validation = Some(validation);
        }
    }

    impl Drop for HomeGuardTestScope {
        fn drop(&mut self) {
            self.restore_real_environment_and_assert();
            if let Some(validation) = &self.unwind_validation {
                validation.store(true, Ordering::SeqCst);
            }
        }
    }

    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl EnvRestore {
        fn capture(names: &[&'static str]) -> Self {
            Self(
                names
                    .iter()
                    .map(|name| (*name, std::env::var_os(name)))
                    .collect(),
            )
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    fn command_env(command: &Command, name: &str) -> Option<Option<std::ffi::OsString>> {
        command.get_envs().find_map(|(key, value)| {
            (key == std::ffi::OsStr::new(name)).then(|| value.map(std::ffi::OsStr::to_os_string))
        })
    }

    #[test]
    fn hermetic_contexts_isolate_concurrent_fixture_roots() {
        let contexts = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                let context = HermeticTestContext::new();
                let command = context.command(TestBinary::CurrentTest);
                (
                    context.root().to_path_buf(),
                    command_env(&command, "HOME").expect("HOME override"),
                    command_env(&command, crate::paths::HOMEBOY_DATA_DIR_ENV)
                        .expect("data override"),
                    command_env(&command, crate::paths::DAEMON_STATE_DIR_ENV)
                        .expect("daemon override"),
                    command_env(
                        &command,
                        crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
                    )
                    .expect("invocation runtime override"),
                )
            });
            let second = scope.spawn(|| {
                let context = HermeticTestContext::new();
                let command = context.command(TestBinary::CurrentTest);
                (
                    context.root().to_path_buf(),
                    command_env(&command, "HOME").expect("HOME override"),
                    command_env(&command, crate::paths::HOMEBOY_DATA_DIR_ENV)
                        .expect("data override"),
                    command_env(&command, crate::paths::DAEMON_STATE_DIR_ENV)
                        .expect("daemon override"),
                    command_env(
                        &command,
                        crate::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
                    )
                    .expect("invocation runtime override"),
                )
            });
            (
                first.join().expect("first context"),
                second.join().expect("second context"),
            )
        });

        let (first, second) = contexts;
        assert_ne!(first.0, second.0);
        assert_ne!(first.1, second.1, "HOME must be worktree-scoped");
        assert_ne!(
            first.2, second.2,
            "data and artifact state must be worktree-scoped"
        );
        assert_ne!(first.3, second.3, "daemon state must be worktree-scoped");
        assert_ne!(
            first.4, second.4,
            "socket-capable invocation runtime must be worktree-scoped"
        );
    }

    #[cfg(unix)]
    fn wait_for_pid_file(path: &Path) -> libc::pid_t {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        fs::read_to_string(path)
            .expect("descendant pid fixture")
            .trim()
            .parse()
            .expect("numeric descendant pid")
    }

    #[cfg(unix)]
    fn assert_pid_reaped(pid: libc::pid_t) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while crate::process::pid_is_running(pid as u32) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !crate::process::pid_is_running(pid as u32),
            "hermetic cleanup left descendant {pid} alive"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_output_timeout_reaps_term_resistant_descendants() {
        let _lock = env_lock();
        let _budget = EnvRestore::capture(&[HERMETIC_SUBPROCESS_BUDGET_ENV]);
        std::env::set_var(HERMETIC_SUBPROCESS_BUDGET_ENV, "1");
        let temp = tempfile::tempdir().expect("tempdir");
        let descendant = temp.path().join("descendant.pid");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            &format!(
                "trap '' TERM; sh -c 'trap \"\" TERM; while :; do :; done' & echo $! > {}; wait",
                crate::engine::shell::quote_path(&descendant.display().to_string())
            ),
        ]);

        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bounded_output(command)));
        assert!(result.is_err(), "timeout must fail the owning test");
        assert_pid_reaped(wait_for_pid_file(&descendant));
    }

    #[test]
    fn hermetic_commands_keep_daemons_in_the_test_runner_process_group() {
        let command = HermeticTestContext::new().command(TestBinary::CurrentTest);
        assert_eq!(
            command_env(&command, TEST_KEEP_DAEMON_IN_PROCESS_GROUP_ENV),
            Some(Some("1".into()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn hermetic_runner_clean_exits_do_not_pay_descendant_cleanup_grace_period() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let runner = workspace.join("scripts/nextest-hermetic-test-environment.sh");
        let started = Instant::now();
        for _ in 0..4 {
            let status = Command::new("sh")
                .arg(&runner)
                .arg("true")
                .status()
                .expect("run hermetic test runner");
            assert!(status.success());
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "clean test processes must not each pay the one-second descendant cleanup grace period"
        );
    }

    /// The runner must hand the test a temp root it can execute from (#12345),
    /// and that root must not sit inside the repository (#12377).
    ///
    /// It used to `unset TMPDIR TMP TEMP`, dropping every test onto the shared
    /// system `/tmp`. On a host that mounts `/tmp` noexec that is not a loud
    /// failure: bash's PATH search calls `access(X_OK)`, Linux fails it on a
    /// noexec mount, so a mock executable written into the temp dir is SKIPPED
    /// and the REAL binary further down PATH runs instead. The test then
    /// asserts against the real tool's output without ever reporting that its
    /// fixture did not exist.
    ///
    /// The first fix put the root under `target/`, which is exec-capable by
    /// construction but is inside a git checkout -- and code that walks up
    /// looking for a repository root then escapes the temp directory and finds
    /// the real one. So the root is now a PROBED system temp directory, and
    /// `target/` is only the last resort. The runner reports which it chose.
    ///
    /// Pinned host-independently by the TMPDIR value, not by probing /tmp: on a
    /// host where /tmp happens to be executable the old behavior would pass an
    /// execution-only check, and the regression would stay invisible exactly
    /// where CI runs.
    #[cfg(unix)]
    #[test]
    fn hermetic_runner_provides_an_executable_temp_root_outside_the_repository() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let runner = workspace.join("scripts/nextest-hermetic-test-environment.sh");

        let output = Command::new("sh")
            .arg(&runner)
            .args([
                "sh",
                "-c",
                r#"printf 'tmpdir=%s\ntmp=%s\ntemp=%s\nsource=%s\n' \
                     "$TMPDIR" "$TMP" "$TEMP" "$HOMEBOY_TEST_TMP_SOURCE"
                   printf '#!/bin/sh\necho fixture-executed\n' > "$TMPDIR/fixture"
                   chmod +x "$TMPDIR/fixture"
                   "$TMPDIR/fixture""#,
            ])
            // The caller's environment must not decide the answer. CI exports
            // no TMPDIR, which is the case that regressed.
            .env_remove("TMPDIR")
            .env_remove("TMP")
            .env_remove("TEMP")
            .output()
            .expect("run hermetic test runner");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let field = |name: &str| {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .unwrap_or_default()
                .to_string()
        };
        let tmpdir = field("tmpdir=");

        assert!(
            !tmpdir.is_empty(),
            "TMPDIR must be set; unsetting it silently falls back to the shared \
             system /tmp: {stdout}"
        );
        for spelling in ["tmp=", "temp="] {
            assert_eq!(
                field(spelling),
                tmpdir,
                "all three temp spellings must name the same root, or code \
                 reading {spelling} lands somewhere else than code reading TMPDIR"
            );
        }
        assert!(
            stdout.contains("fixture-executed"),
            "an executable written into the temp root must run: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_ne!(
            Path::new(&tmpdir).parent(),
            Some(Path::new("/tmp")),
            "the root must be private to this invocation, not the shared system \
             temp directory itself: {tmpdir}"
        );

        // The in-tree fallback exists for hosts where nothing else can execute.
        // Wherever a system temp directory works, the root must stay outside the
        // checkout -- a temp workspace nested in a git repo is not equivalent to
        // one in /tmp, because repository-root walks escape it (#12377).
        if field("source=") == "system" {
            assert!(
                !Path::new(&tmpdir).starts_with(workspace),
                "a probed system temp root must not live inside the repository: {tmpdir}"
            );
        }

        assert!(
            !Path::new(&tmpdir).exists(),
            "the per-invocation temp root must be removed when the test exits, \
             or it accumulates: {tmpdir}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn hermetic_runner_does_not_nest_under_an_outer_invocation_tmpdir() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let runner = workspace.join("scripts/nextest-hermetic-test-environment.sh");
        let outer = tempfile::tempdir().expect("outer invocation tmpdir");
        let output = Command::new("sh")
            .arg(runner)
            .args(["sh", "-c", "printf '%s' \"$TMPDIR\""])
            .env("TMPDIR", outer.path())
            .output()
            .expect("run hermetic test runner");

        assert!(output.status.success());
        let selected = PathBuf::from(String::from_utf8(output.stdout).expect("UTF-8 tmpdir"));
        assert!(
            !selected.starts_with(outer.path()),
            "test temp state must not be owned by the outer Homeboy invocation: {}",
            selected.display()
        );
        assert!(
            !selected.exists(),
            "the per-test temp root must be removed after execution: {}",
            selected.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn hermetic_runner_reaps_descendants_after_a_panicking_test_binary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let descendant = temp.path().join("descendant.pid");
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let runner = workspace.join("scripts/nextest-hermetic-test-environment.sh");
        let mut command = Command::new("sh");
        command
            .arg(runner)
            .args([
                "sh",
                "-c",
                &format!(
                    "trap '' TERM; sh -c 'trap \"\" TERM; while :; do :; done' & echo $! > {}; exit 101",
                    crate::engine::shell::quote_path(&descendant.display().to_string())
                ),
            ]);
        let output = command.output().expect("run hermetic test runner");
        assert_eq!(output.status.code(), Some(101));
        assert_pid_reaped(wait_for_pid_file(&descendant));
    }

    #[cfg(unix)]
    #[test]
    fn hermetic_runner_reaps_descendants_after_a_successful_test_binary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let descendant = temp.path().join("descendant.pid");
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let runner = workspace.join("scripts/nextest-hermetic-test-environment.sh");
        let mut command = Command::new("sh");
        command.arg(runner).args([
            "sh",
            "-c",
            &format!(
                "trap '' TERM; sh -c 'trap \"\" TERM; while :; do :; done' & echo $! > {}; exit 0",
                crate::engine::shell::quote_path(&descendant.display().to_string())
            ),
        ]);
        let output = command.output().expect("run hermetic test runner");
        assert!(output.status.success());
        assert_pid_reaped(wait_for_pid_file(&descendant));
    }

    #[test]
    fn hermetic_commands_clear_inherited_lab_transport_metadata() {
        let _lock = env_lock();
        let source = crate::observation::SOURCE_SNAPSHOT_METADATA_ENV;
        let lab = crate::observation::LAB_OFFLOAD_METADATA_ENV;
        let _restore = EnvRestore::capture(&[source, lab]);

        for (source_value, lab_value) in [
            (Some(r#"{"source":"only"}"#), None),
            (None, Some(r#"{"lab":"only"}"#)),
            (Some(r#"{"source":"paired"}"#), Some(r#"{"lab":"paired"}"#)),
        ] {
            match source_value {
                Some(value) => std::env::set_var(source, value),
                None => std::env::remove_var(source),
            }
            match lab_value {
                Some(value) => std::env::set_var(lab, value),
                None => std::env::remove_var(lab),
            }

            let command = HermeticTestContext::new().command(TestBinary::CurrentTest);
            assert_eq!(command_env(&command, source), Some(None));
            assert_eq!(command_env(&command, lab), Some(None));
        }
    }

    #[test]
    fn reverse_broker_fixture_projects_active_and_stale_runner_jobs() {
        let store = JobStore::default();
        let active = store
            .submit_runner_api_fixture(
                serde_json::from_value(serde_json::json!({
                    "runner_id": "lab",
                    "command": ["true"],
                }))
                .expect("active runner request"),
            )
            .expect("queue active runner job");
        let response = handle_reverse_broker_request(
            &store,
            "lab",
            ReverseBrokerRequest {
                method: "GET".to_string(),
                path: "/jobs".to_string(),
                body: serde_json::Value::Null,
            },
        );

        assert_eq!(response["success"], true);
        assert_eq!(
            response["data"]["body"]["jobs"][0]["id"],
            active.id.to_string()
        );
        assert_eq!(response["data"]["body"]["active_runner_job_count"], 1);
        assert_eq!(
            response["data"]["body"]["active_runner_jobs"][0]["job_id"],
            active.id.to_string()
        );
        assert_eq!(response["data"]["body"]["stale_runner_job_count"], 0);

        let temp = tempfile::tempdir().expect("job store directory");
        let path = temp.path().join("jobs.json");
        let store = JobStore::open(&path).expect("open durable store");
        let stale = store
            .submit_runner_api_fixture(
                serde_json::from_value(serde_json::json!({
                    "runner_id": "lab",
                    "command": ["true"],
                }))
                .expect("stale runner request"),
            )
            .expect("queue stale runner job");
        store
            .claim_remote_runner_job("lab", None, 30_000, None)
            .expect("claim stale runner job");
        let recovered = JobStore::open(&path).expect("recover stale runner job");
        let response = handle_reverse_broker_request(
            &recovered,
            "lab",
            ReverseBrokerRequest {
                method: "GET".to_string(),
                path: "/jobs".to_string(),
                body: serde_json::Value::Null,
            },
        );

        assert_eq!(response["data"]["body"]["stale_runner_job_count"], 1);
        assert_eq!(
            response["data"]["body"]["stale_runner_jobs"][0]["job_id"],
            stale.id.to_string()
        );
    }

    #[test]
    fn hermetic_commands_replace_inherited_daemon_and_data_roots() {
        let _lock = env_lock();
        let data = crate::paths::HOMEBOY_DATA_DIR_ENV;
        let daemon = crate::paths::DAEMON_STATE_DIR_ENV;
        let _restore = EnvRestore::capture(&[data, daemon, TEST_DAEMON_NAMESPACE_ENV]);
        std::env::set_var(data, "/operator/homeboy-data");
        std::env::set_var(daemon, "/operator/homeboy-daemon");

        let context = HermeticTestContext::new();
        let command = context.command(TestBinary::CurrentTest);

        assert_eq!(
            command_env(&command, data),
            Some(Some(context.data_dir().into_os_string()))
        );
        assert_eq!(
            command_env(&command, daemon),
            Some(Some(context.daemon_dir().into_os_string()))
        );
        assert_eq!(
            command_env(&command, TEST_DAEMON_NAMESPACE_ENV),
            Some(Some(context.daemon_dir().into_os_string()))
        );
    }

    #[test]
    fn home_guard_owns_xdg_and_preserves_process_temp_roots() {
        let names = [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_RUNTIME_DIR",
            "TMPDIR",
            "TEMP",
            "TMP",
            crate::paths::HOMEBOY_DATA_DIR_ENV,
        ];
        let mut scope = HomeGuardTestScope::new(&names, || {
            for name in names {
                std::env::set_var(name, format!("/ambient/{name}"));
            }
        });

        let expected = {
            let guard = scope.home();
            let root = guard.context.home().to_path_buf();
            assert_eq!(
                crate::paths::homeboy_data().expect("isolated data root"),
                root.join(".local/share/homeboy")
            );
            for (name, path) in [
                ("XDG_CONFIG_HOME", root.join(".config")),
                ("XDG_CACHE_HOME", root.join(".cache")),
                ("XDG_DATA_HOME", root.join(".local/share")),
                ("XDG_STATE_HOME", root.join(".local/state")),
                ("XDG_RUNTIME_DIR", guard.context.runtime_dir().to_path_buf()),
            ] {
                assert_eq!(
                    std::env::var_os(name),
                    Some(path.into_os_string()),
                    "{name}"
                );
            }
            let command = guard.context.command(TestBinary::CurrentTest);
            for name in ["TMPDIR", "TEMP", "TMP"] {
                assert_eq!(
                    std::env::var(name).expect("ambient temp root"),
                    format!("/ambient/{name}"),
                    "{name} must remain process-global"
                );
                assert_eq!(
                    command_env(&command, name),
                    Some(Some(guard.context.temp_dir().into_os_string())),
                    "{name} must be isolated for subprocesses"
                );
            }
            assert!(std::env::var_os(crate::paths::HOMEBOY_DATA_DIR_ENV).is_none());
            root
        };

        scope.restore_isolated_home();

        assert!(
            !expected.exists(),
            "the isolated root is dropped after restoration"
        );
        for name in names {
            assert_eq!(
                std::env::var(name).expect("sentinel environment"),
                format!("/ambient/{name}"),
                "{name} was restored to its immediate sentinel"
            );
        }

        scope.restore_real_environment_and_assert();
    }

    #[test]
    fn home_guard_test_scope_restores_the_real_environment_after_unwind() {
        const NAME: &str = "__HOMEBOY_TEST_SCOPE_UNWIND__";
        let validated = Arc::new(AtomicBool::new(false));

        let panicked = std::panic::catch_unwind({
            let validated = Arc::clone(&validated);
            move || {
                let mut scope = HomeGuardTestScope::new(&[NAME], || {
                    std::env::set_var(NAME, "sentinel");
                });
                scope.record_unwind_validation(validated);
                panic!("exercise scope unwind");
            }
        })
        .is_err();

        assert!(panicked);
        assert!(validated.load(Ordering::SeqCst));
    }

    #[cfg(unix)]
    #[test]
    fn current_test_controller_fixture_answers_with_the_cli_identity() {
        let source = test_controller_fixture_source(TestBinary::CurrentTest);
        let output = Command::new(source)
            .arg("--version")
            .output()
            .expect("run controller version fixture");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout)
                .expect("version output")
                .trim(),
            crate::build_identity::current().display
        );
    }

    #[test]
    fn hermetic_commands_allow_explicit_complete_lab_transport_metadata() {
        let context = HermeticTestContext::new();
        let source = crate::observation::SOURCE_SNAPSHOT_METADATA_ENV;
        let lab = crate::observation::LAB_OFFLOAD_METADATA_ENV;
        let source_value = r#"{"source":"fixture"}"#;
        let lab_value = r#"{"lab":"fixture"}"#;
        let mut command = context.command(TestBinary::CurrentTest);
        command.env(source, source_value).env(lab, lab_value);

        assert_eq!(
            command_env(&command, source),
            Some(Some(source_value.into()))
        );
        assert_eq!(command_env(&command, lab), Some(Some(lab_value.into())));
    }

    #[cfg(unix)]
    #[test]
    fn cached_exec_base_probes_once_and_creates_distinct_tempdirs() {
        let base = TempDir::new().expect("temp base");
        let cache = Mutex::new(None);
        let probes = std::sync::atomic::AtomicUsize::new(0);
        let create = || {
            tempdir_with_cached_exec_base(
                &cache,
                vec![base.path().to_path_buf()],
                "hb-cache-",
                |_| {
                    probes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    true
                },
            )
        };

        let first = create();
        let second = create();

        assert_eq!(probes.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_ne!(first.path(), second.path());
        assert!(first.path().starts_with(base.path()));
        assert!(second.path().starts_with(base.path()));
    }

    #[cfg(unix)]
    #[test]
    fn sweep_removes_stale_hb_test_dirs_and_spares_fresh_and_foreign() {
        use std::os::unix::fs::PermissionsExt;

        let root = TempDir::new().expect("sweep root");

        // Stale leaked tempdir: matching prefix, mtime well past the threshold.
        let stale = root.path().join("hb-test-STALE");
        fs::create_dir(&stale).expect("create stale dir");
        fs::write(stale.join("leaked.txt"), b"leaked").expect("write into stale dir");
        // Backdate mtime to just beyond LEAKED_TEMPDIR_MAX_AGE.
        let past = std::time::SystemTime::now()
            - (LEAKED_TEMPDIR_MAX_AGE + std::time::Duration::from_secs(60));
        set_dir_mtime(&stale, past);

        // Fresh tempdir from a concurrent run: matching prefix, current mtime.
        let fresh = root.path().join("hb-test-FRESH");
        fs::create_dir(&fresh).expect("create fresh dir");

        // Foreign directory: old but does not match the hb-test- prefix.
        let foreign = root.path().join("someones-important-data");
        fs::create_dir(&foreign).expect("create foreign dir");
        set_dir_mtime(&foreign, past);

        // A matching-prefix *file* (not a dir) must be left alone.
        let stray_file = root.path().join("hb-test-not-a-dir");
        fs::write(&stray_file, b"file").expect("write stray file");
        let _ = fs::set_permissions(&stray_file, fs::Permissions::from_mode(0o644));

        sweep_leaked_test_tempdirs(std::slice::from_ref(&root.path().to_path_buf()), None);

        assert!(!stale.exists(), "stale hb-test- dir should be swept");
        assert!(fresh.exists(), "fresh hb-test- dir must be spared");
        assert!(foreign.exists(), "non-hb-test- dir must never be touched");
        assert!(stray_file.exists(), "matching-prefix file must be spared");
    }

    #[cfg(unix)]
    fn set_dir_mtime(path: &Path, when: std::time::SystemTime) {
        // Best-effort mtime backdating via `touch -d`. Skips silently if the
        // platform `touch` is unavailable; the assertion on `stale` would then
        // catch a real regression on hosts where the sweep must work.
        let secs = when
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = Command::new("touch")
            .arg("-d")
            .arg(format!("@{secs}"))
            .arg(path)
            .status();
    }

    #[test]
    fn isolated_homes_share_the_read_only_controller_fixture_and_not_pins() {
        let first = with_isolated_home(|_| {
            let fixture = controller_runtime_test_executable();
            let runtime = crate::controller_runtime::pin_current_in_root(
                &crate::controller_runtime::runtime_root_in(
                    &crate::paths::PathRoots::from_environment()
                        .expect("path roots")
                        .data()
                        .to_path_buf(),
                )
                .expect("runtime root"),
            )
            .expect("pin first fixture");
            let pin = runtime["originating"]["pinned_executable"]
                .as_str()
                .map(PathBuf::from)
                .expect("first pinned fixture");
            (fixture, pin)
        });
        let second = with_isolated_home(|_| {
            let fixture = controller_runtime_test_executable();
            let runtime = crate::controller_runtime::pin_current_in_root(
                &crate::controller_runtime::runtime_root_in(
                    &crate::paths::PathRoots::from_environment()
                        .expect("path roots")
                        .data()
                        .to_path_buf(),
                )
                .expect("runtime root"),
            )
            .expect("pin second fixture");
            let pin = runtime["originating"]["pinned_executable"]
                .as_str()
                .map(PathBuf::from)
                .expect("second pinned fixture");
            (fixture, pin)
        });

        assert_eq!(first.0, second.0);
        assert_ne!(
            first.0,
            std::env::current_exe().expect("current test executable")
        );
        assert_ne!(first.1, second.1);
        assert!(first.0.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = fs::metadata(&first.0)
                .expect("fixture metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o222, 0);
            assert_ne!(mode & 0o111, 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn tempdir_names_carry_their_owning_pid() {
        let prefix = owned_tempdir_prefix();
        assert!(prefix.starts_with(TEST_TEMPDIR_PREFIX));
        assert_eq!(
            tempdir_owner_pid(&format!("{prefix}AbCdEf")),
            Some(std::process::id())
        );
    }

    /// The marker is the only thing that makes a leaked home recognizable after
    /// its process is gone, so *every* allocation path has to carry it —
    /// including the fallback taken when no candidate root passes the exec
    /// probe. That path used to hand back an unmarked `.tmpXXXXXX` directory,
    /// which no sweep and no cleanup category could attribute (#11073).
    #[cfg(unix)]
    #[test]
    fn every_allocation_path_marks_its_tempdir_including_the_fallback() {
        // Force the fallback: one candidate root that exists, and a probe that
        // refuses it, so the ordered loop exhausts without a match.
        let base = TempDir::new().expect("temp base");
        let cache = Mutex::new(None);
        let fallback = tempdir_with_cached_exec_base(
            &cache,
            vec![base.path().to_path_buf()],
            &owned_tempdir_prefix(),
            |_| false,
        );

        let probed = exec_capable_tempdir();

        for directory in [fallback.path(), probed.path()] {
            let name = directory
                .file_name()
                .and_then(|name| name.to_str())
                .expect("tempdir name");
            assert!(
                name.starts_with(TEST_TEMPDIR_PREFIX),
                "{name} carries no ownership marker"
            );
            assert_eq!(
                tempdir_owner_pid(name),
                Some(std::process::id()),
                "{name} does not name its owner"
            );
        }
    }

    /// Directories written before the PID prefix existed must stay on the age
    /// heuristic rather than being misread as owned by PID-less garbage.
    #[cfg(unix)]
    #[test]
    fn legacy_and_malformed_tempdir_names_report_no_owner() {
        for name in [
            "hb-test-AbCdEf",
            "hb-test-",
            "hb-test-notapid-AbCdEf",
            "hb-test--AbCdEf",
            "something-else",
        ] {
            assert_eq!(tempdir_owner_pid(name), None, "{name} must have no owner");
        }
    }

    /// The reclaim contract: a dead owner's directory goes, a live owner's
    /// stays, and neither decision waits on a clock.
    #[cfg(unix)]
    #[test]
    fn the_sweep_reclaims_dead_owners_and_spares_live_ones() {
        let root = tempfile::tempdir().expect("sweep root");

        // A conclusively dead PID: spawn a child, reap it, and reuse its id.
        // Not PID 0 — `kill(0, 0)` addresses the caller's whole process group
        // and therefore reports *alive*, which would make this test vacuous.
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn short-lived child");
        let dead_pid = child.id();
        child.wait().expect("reap short-lived child");
        assert!(
            !crate::process::pid_is_running(dead_pid),
            "reaped child must read as dead"
        );
        let dead = root
            .path()
            .join(format!("{TEST_TEMPDIR_PREFIX}{dead_pid}-deadxx"));
        let active = root
            .path()
            .join(format!("{TEST_TEMPDIR_PREFIX}{dead_pid}-active"));
        let active_tmp = active.join("tmp");
        // This process is unambiguously alive.
        let live = root.path().join(owned_tempdir_prefix() + "livexx");
        // No PID segment and freshly created — the age fallback must keep it.
        let legacy_fresh = root.path().join("hb-test-legacyx");
        // Not ours at all.
        let unrelated = root.path().join("someone-elses-dir");

        for path in [&dead, &active_tmp, &live, &legacy_fresh, &unrelated] {
            fs::create_dir_all(path).expect("seed sweep fixture");
            fs::write(path.join("payload"), b"x").expect("seed payload");
        }

        sweep_leaked_test_tempdirs(&[root.path().to_path_buf()], Some(&active_tmp));

        assert!(!dead.exists(), "a dead owner's tempdir must be reclaimed");
        assert!(
            active.exists(),
            "the active TMPDIR hierarchy must never be reclaimed"
        );
        assert!(live.exists(), "a live owner's tempdir must be preserved");
        assert!(
            legacy_fresh.exists(),
            "a recent PID-less tempdir must survive on the age heuristic"
        );
        assert!(
            unrelated.exists(),
            "unrelated directories must be untouched"
        );
    }

    /// The PID segment lengthens the invocation runtime root, which is the
    /// budget `sockaddr_un` is measured against. Pin that it still fits, so a
    /// future change to the naming cannot silently push socket paths over.
    #[cfg(unix)]
    #[test]
    fn the_invocation_tempdir_still_fits_the_socket_budget() {
        let dir = short_invocation_tempdir();
        crate::engine::invocation::enforce_path_budget(&dir.path().join(TEST_INVOCATION_ID))
            .expect("invocation runtime plus its leaf must leave room for a workload socket name");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_invocation_temp_base_keeps_the_state_leaf_within_the_socket_budget() {
        let max_state_leaf_len = crate::engine::invocation::SUN_PATH_CAPACITY
            - crate::engine::invocation::SOCKET_HEADROOM_BYTES
            - 1;
        let allocated_root_suffix_len = 1
            + owned_tempdir_prefix().len()
            + TEMPFILE_RANDOM_SUFFIX_BYTES
            + 1
            + TEST_INVOCATION_ID.len();
        let max_base_len = max_state_leaf_len - allocated_root_suffix_len;
        let accepted = PathBuf::from(format!("/{}", "a".repeat(max_base_len - 1)));
        let rejected = PathBuf::from(format!("/{}", "a".repeat(max_base_len)));

        assert!(
            invocation_temp_base_fits(&accepted),
            "a base whose allocated state leaf is exactly at the budget must fit"
        );
        assert!(
            !invocation_temp_base_fits(&rejected),
            "a base whose allocated state leaf exceeds the budget must be rejected"
        );

        let fixture = tempfile::tempdir_in("/tmp").expect("short fixture root");
        let fixture_len = fixture.path().to_string_lossy().len();
        assert!(
            fixture_len < max_base_len,
            "fixture root must leave room for the modeled base"
        );
        let base = fixture
            .path()
            .join("a".repeat(max_base_len - fixture_len - 1));
        fs::create_dir(&base).expect("create exact-budget base");
        let state_root = tempfile::Builder::new()
            .prefix(&owned_tempdir_prefix())
            .tempdir_in(&base)
            .expect("create PID-owned state root");
        let suffix = state_root
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(&owned_tempdir_prefix()))
            .expect("state root retains its PID-owned prefix");
        assert_eq!(
            suffix.len(),
            TEMPFILE_RANDOM_SUFFIX_BYTES,
            "tempfile suffix length must match the modeled allocation shape"
        );
        assert!(
            invocation_runtime_dir_fits(state_root.path()),
            "an allocated state leaf at the budget must fit"
        );
    }
}
