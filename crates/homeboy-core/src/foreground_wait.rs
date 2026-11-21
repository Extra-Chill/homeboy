//! A foreground client's observable, bounded and interruptible wait for
//! remote capacity (#18042).
//!
//! A foreground command such as `agent-task recover` running replacement gates
//! can block on Lab admission: a runner refusing for pressure or capacity, or a
//! submitted gate job queued behind the daemon's capacity limit. That wait was
//! silent, had no bound while the job stayed queued, and ignored SIGTERM once
//! the process-group cleanup handler had replaced the default disposition.
//!
//! Inside [`with_foreground_wait`] a wait instead
//! - reports what it waits for on stderr: phase, runner, reason, queue
//!   position when known, runner job and elapsed time;
//! - stops once one admission wait exceeds the declared bound; and
//! - stops on SIGINT/SIGTERM received after the scope began, during the
//!   admission wait or while the admitted work runs.
//!
//! Waiters call [`waiting`] while they wait, [`admitted`] once the work starts,
//! and poll [`stop_requested`]. When it answers, they withdraw queued work and
//! return; the caller turns the stop into [`WaitStop::error`]. Outside a scope
//! every function is a no-op, so daemon-owned work keeps its own budgets.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::{Error, ErrorCode};

/// How often a continuing wait repeats its progress line.
const REPORT_INTERVAL: Duration = Duration::from_secs(30);

/// Limits a foreground client declares for its waits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForegroundWaitPolicy {
    /// Longest one admission wait may last before it stops. `None` keeps the
    /// waiter's own budget.
    pub admission_timeout: Option<Duration>,
}

/// What a wait is waiting for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WaitSubject {
    /// `lab_admission` (the runner refused admission) or `runner_queue` (the
    /// submitted job is queued on the runner).
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    /// Admission reason, such as `capacity_reached` or `pressure`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
}

impl WaitSubject {
    /// A submitted runner job still queued for runner capacity.
    pub fn runner_queue(runner_id: &str, job_id: &str) -> Self {
        Self {
            phase: "runner_queue".to_string(),
            runner_id: Some(runner_id.to_string()),
            reason: Some("queued_for_runner_capacity".to_string()),
            queue_position: None,
            job_id: Some(job_id.to_string()),
        }
    }

    /// A transient admission refusal, read from its structured details.
    pub fn from_refusal(label: &str, error: &Error) -> Self {
        let details = &error.details;
        let first = |value: &serde_json::Value| {
            value
                .as_array()
                .and_then(|reasons| reasons.iter().find_map(|reason| reason.as_str()))
                .map(str::to_string)
        };
        let reason = first(&details["reasons"])
            .or_else(|| first(&details["runner_availability"]["reasons"]))
            .or_else(|| first(&details["readiness_reasons"]))
            .or_else(|| details["substrate_wait"].as_str().map(str::to_string))
            .or_else(|| details["admission_wait"].as_str().map(str::to_string))
            .unwrap_or_else(|| error.code.as_str().to_string());
        Self {
            phase: "lab_admission".to_string(),
            runner_id: details["runner_id"]
                .as_str()
                .or_else(|| details["runner_availability"]["runner_id"].as_str())
                .map(str::to_string),
            reason: Some(reason),
            queue_position: details["queue_position"].as_u64(),
            job_id: details["job_id"].as_str().map(str::to_string),
        }
        .labelled(label)
    }

    fn labelled(mut self, label: &str) -> Self {
        if self.runner_id.is_none() && self.reason.is_none() {
            self.reason = Some(label.to_string());
        }
        self
    }

    fn describe(&self) -> String {
        let what = match self.phase.as_str() {
            "runner_queue" => "runner capacity",
            _ => "Lab admission",
        };
        let mut line = format!("waiting for {what}");
        if let Some(runner) = &self.runner_id {
            line.push_str(&format!(" on runner `{runner}`"));
        }
        let mut facts = Vec::new();
        if let Some(reason) = &self.reason {
            facts.push(reason.clone());
        }
        if let Some(position) = self.queue_position {
            facts.push(format!("queue position {position}"));
        }
        if let Some(job) = &self.job_id {
            facts.push(format!("job {job}"));
        }
        if !facts.is_empty() {
            line.push_str(&format!(" ({})", facts.join(", ")));
        }
        line
    }
}

/// Why a foreground wait stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitStop {
    /// SIGINT or SIGTERM arrived after the scope began.
    Signal(i32),
    /// One admission wait outlasted the declared bound.
    AdmissionTimeout { waited: Duration, limit: Duration },
}

impl WaitStop {
    /// Process exit status a signal stop conventionally reports.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Signal(signal) => 128 + signal,
            Self::AdmissionTimeout { .. } => 124,
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Signal(signal) => format!("interrupted by signal {}", signal_name(*signal)),
            Self::AdmissionTimeout { waited, limit } => format!(
                "admission wait timed out after {}s (--wait-timeout-seconds {})",
                waited.as_secs(),
                limit.as_secs()
            ),
        }
    }

    /// The error a foreground client returns for this stop. `subject` is the
    /// last wait observed, so the operator sees what it was waiting for;
    /// `consequence` states what the stop left behind.
    pub fn error(
        &self,
        operation: &str,
        subject: Option<&WaitSubject>,
        consequence: &str,
    ) -> Error {
        let waiting = subject
            .map(|subject| format!(" while {}", subject.describe()))
            .unwrap_or_default();
        let mut error = Error::new(
            ErrorCode::ValidationInvalidArgument,
            format!(
                "{operation} stopped: {}{waiting}; {consequence}",
                self.describe()
            ),
            serde_json::json!({
                "field": "foreground_wait",
                "foreground_wait": {
                    "stop": match self {
                        Self::Signal(_) => "signal",
                        Self::AdmissionTimeout { .. } => "admission_timeout",
                    },
                    "signal": match self {
                        Self::Signal(signal) => Some(*signal),
                        Self::AdmissionTimeout { .. } => None,
                    },
                    "waited_seconds": match self {
                        Self::AdmissionTimeout { waited, .. } => Some(waited.as_secs()),
                        Self::Signal(_) => None,
                    },
                    "subject": subject,
                    "exit_code": self.exit_code(),
                },
            }),
        );
        error.retryable = Some(true);
        error
    }
}

fn signal_name(signal: i32) -> String {
    match signal {
        2 => "SIGINT".to_string(),
        15 => "SIGTERM".to_string(),
        other => other.to_string(),
    }
}

type Sink = Arc<dyn Fn(&str) + Send + Sync>;

struct State {
    policy: ForegroundWaitPolicy,
    /// SIGINT/SIGTERM count when the scope began; a later signal stops it.
    signals_at_entry: usize,
    observer_installed: bool,
    current: Option<(WaitSubject, Instant)>,
    last_subject: Option<WaitSubject>,
    last_report: Option<Instant>,
    stop: Option<WaitStop>,
    sink: Sink,
}

/// One foreground client's wait scope.
pub struct ForegroundWait {
    state: Mutex<State>,
}

impl ForegroundWait {
    fn new(policy: ForegroundWaitPolicy, sink: Sink) -> Self {
        Self {
            state: Mutex::new(State {
                policy,
                signals_at_entry: crate::server::process_cleanup::termination_signals().0,
                observer_installed: false,
                current: None,
                last_subject: None,
                last_report: None,
                stop: None,
                sink,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Start counting SIGINT/SIGTERM for this scope. Idempotent. Once the
    /// observer is installed a signal no longer terminates the process by
    /// default, so only a scope that polls [`Self::stop_requested`] calls it.
    pub fn observe_signals(&self) {
        let mut state = self.lock();
        Self::observe_signals_locked(&mut state);
    }

    fn observe_signals_locked(state: &mut State) {
        if !state.observer_installed {
            state.observer_installed = true;
            crate::server::process_cleanup::install_termination_observer();
        }
    }

    /// Record that the client is waiting for `subject`. The first wait
    /// installs the termination observer, so SIGINT/SIGTERM keep their
    /// default disposition until something actually waits.
    pub fn waiting(&self, subject: WaitSubject) {
        let mut state = self.lock();
        Self::observe_signals_locked(&mut state);
        let now = Instant::now();
        let changed = state
            .current
            .as_ref()
            .is_none_or(|(current, _)| current != &subject);
        let started = state.current.as_ref().map_or(now, |(_, started)| *started);
        state.current = Some((subject.clone(), started));
        state.last_subject = Some(subject.clone());
        let due = state
            .last_report
            .is_none_or(|last| now.duration_since(last) >= REPORT_INTERVAL);
        if changed || due {
            state.last_report = Some(now);
            let mut line = format!(
                "homeboy: {} — waited {}s",
                subject.describe(),
                now.duration_since(started).as_secs()
            );
            if let Some(limit) = state.policy.admission_timeout {
                line.push_str(&format!(" of {}s", limit.as_secs()));
            }
            line.push_str("; SIGINT/SIGTERM withdraws the queued work");
            (state.sink)(&line);
        }
    }

    /// The awaited work started; the next wait is a new admission wait.
    pub fn admitted(&self) {
        self.lock().current = None;
    }

    /// Whether the client must stop: a signal since the scope's first wait,
    /// or an admission wait past its bound. Sticky once answered.
    pub fn stop_requested(&self) -> Option<WaitStop> {
        let mut state = self.lock();
        if state.stop.is_none() {
            let (count, signal) = crate::server::process_cleanup::termination_signals();
            if count > state.signals_at_entry {
                state.stop = Some(WaitStop::Signal(signal));
            } else if let (Some(limit), Some((_, started))) =
                (state.policy.admission_timeout, state.current.as_ref())
            {
                let waited = started.elapsed();
                if waited >= limit {
                    state.stop = Some(WaitStop::AdmissionTimeout { waited, limit });
                }
            }
        }
        state.stop.clone()
    }

    /// Stop the scope's waits as if `stop` had been observed.
    pub fn request_stop(&self, stop: WaitStop) {
        self.lock().stop.get_or_insert(stop);
    }

    /// The last thing the scope waited for.
    pub fn last_subject(&self) -> Option<WaitSubject> {
        self.lock().last_subject.clone()
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<ForegroundWait>>> = const { RefCell::new(None) };
}

/// Run `operation` as a foreground client whose waits report to stderr.
pub fn with_foreground_wait<T>(policy: ForegroundWaitPolicy, operation: impl FnOnce() -> T) -> T {
    with_foreground_wait_reporting(policy, Arc::new(|line| eprintln!("{line}")), operation)
}

/// [`with_foreground_wait`] with an explicit progress sink.
pub fn with_foreground_wait_reporting<T>(
    policy: ForegroundWaitPolicy,
    sink: Arc<dyn Fn(&str) + Send + Sync>,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Arc<ForegroundWait>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let scope = Arc::new(ForegroundWait::new(policy, sink));
    let _restore = Restore(CURRENT.with(|slot| slot.replace(Some(scope))));
    operation()
}

/// The active scope on this thread, if any.
pub fn current() -> Option<Arc<ForegroundWait>> {
    CURRENT.with(|slot| slot.borrow().clone())
}

/// Record a wait in the active scope. No-op outside one.
pub fn waiting(subject: WaitSubject) {
    if let Some(scope) = current() {
        scope.waiting(subject);
    }
}

/// Start honouring SIGINT/SIGTERM in the active scope. No-op outside one.
pub fn observe_signals() {
    if let Some(scope) = current() {
        scope.observe_signals();
    }
}

/// The awaited work started. No-op outside a scope.
pub fn admitted() {
    if let Some(scope) = current() {
        scope.admitted();
    }
}

/// Whether the active scope must stop. Always `None` outside a scope.
pub fn stop_requested() -> Option<WaitStop> {
    current().and_then(|scope| scope.stop_requested())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::server::process_cleanup::SIGNAL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn capture() -> (Sink, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink_lines = lines.clone();
        (
            Arc::new(move |line: &str| sink_lines.lock().unwrap().push(line.to_string())),
            lines,
        )
    }

    /// A wait names its runner, reason and job, and a bound stops only an
    /// admission wait, never work that already started.
    #[test]
    fn wait_reports_its_subject_and_its_bound_stops_only_admission() {
        let _signals = signal_lock();
        let (sink, lines) = capture();
        let policy = ForegroundWaitPolicy {
            admission_timeout: Some(Duration::from_millis(20)),
        };
        with_foreground_wait_reporting(policy, sink, || {
            assert_eq!(stop_requested(), None);
            waiting(WaitSubject::runner_queue("lab-a", "job-7"));
            let line = lines.lock().unwrap().last().cloned().unwrap();
            assert!(line.contains("runner `lab-a`"), "{line}");
            assert!(line.contains("queued_for_runner_capacity"), "{line}");
            assert!(line.contains("job job-7"), "{line}");
            admitted();
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(
                stop_requested(),
                None,
                "running work has no admission bound"
            );
            waiting(WaitSubject::runner_queue("lab-a", "job-8"));
            std::thread::sleep(Duration::from_millis(30));
            let stop = stop_requested().expect("bounded admission wait stops");
            assert!(matches!(stop, WaitStop::AdmissionTimeout { .. }));
            let error = stop.error(
                "replacement gates",
                current().unwrap().last_subject().as_ref(),
                "withdrawn",
            );
            assert_eq!(
                error.details["foreground_wait"]["stop"],
                "admission_timeout"
            );
            assert_eq!(
                error.details["foreground_wait"]["subject"]["job_id"],
                "job-8"
            );
        });
        assert_eq!(stop_requested(), None, "outside a scope nothing stops");
    }

    /// #18042 root cause: once the process-group cleanup handler owns
    /// SIGTERM (installed by the first local child spawn), the signal only
    /// records itself, and a wait outside a child wait never saw it. A
    /// scope sees every SIGTERM delivered after it began.
    #[cfg(unix)]
    #[test]
    fn sigterm_after_the_cleanup_handler_owns_it_stops_the_scope() {
        let _signals = signal_lock();
        let (sink, _) = capture();
        with_foreground_wait_reporting(ForegroundWaitPolicy::default(), sink, || {
            observe_signals();
            assert_eq!(stop_requested(), None);
            assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
            let deadline = Instant::now() + Duration::from_secs(10);
            while stop_requested().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let stop = stop_requested().expect("SIGTERM stops the foreground wait");
            assert_eq!(stop, WaitStop::Signal(libc::SIGTERM));
            assert_eq!(stop.exit_code(), 143);
        });
        // Leave no pending interruption for another test's child wait.
        let _ = crate::server::process_cleanup::active_cleanup_signal();
    }

    /// A refusal's runner and admission reason come from its details.
    #[test]
    fn refusal_subject_names_runner_and_reason() {
        let error = Error::new(
            ErrorCode::RunnerLabTransportFailure,
            "refused",
            serde_json::json!({"runner_id": "lab-b", "reasons": ["capacity_reached"]}),
        );
        let subject = WaitSubject::from_refusal("Lab gate execution", &error);
        assert_eq!(subject.phase, "lab_admission");
        assert_eq!(subject.runner_id.as_deref(), Some("lab-b"));
        assert_eq!(subject.reason.as_deref(), Some("capacity_reached"));
    }
}
