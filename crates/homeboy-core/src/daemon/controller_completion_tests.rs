use super::controller_job_driver::{
    ControllerJobDriver, ControllerJobHandle, ControllerJobPublicError,
};
use super::controller_terminal_regression::running_controller;
use super::*;
use serde_json::Value;
use std::sync::atomic::AtomicUsize;

#[test]
fn controller_completion_permanent_rejection_stops_without_retrying_or_rewriting() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path).unwrap();
    let job = store.create("not-a-controller");
    store.start(job.id).unwrap();
    let before = fs::read(&path).unwrap();
    let calls = AtomicUsize::new(0);
    assert!(persist_controller_completion(
        &store.handle(job.id),
        false,
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            store.complete_controller_success(job.id, json!({}))
        }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn controller_completion_cancellation_before_result_retains_the_cancellation_owner() {
    let directory = tempfile::tempdir().unwrap();
    let store = JobStore::open_without_reconciliation(directory.path().join("jobs.json")).unwrap();
    let id = running_controller(&store, "test.cancellation-before-result");
    store
        .request_controller_cancellation(id, "cancel first".to_string())
        .unwrap();
    assert!(!persist_controller_success_or_uncertainty(
        &store.handle(id),
        json!({"late_success": true})
    ));
    persist_controller_cancellation(&store.handle(id));
    assert_eq!(store.get(id).unwrap().status, JobStatus::Cancelled);
    assert!(!store.controller_completion_pending(id).unwrap());
}

struct CompletedDriver {
    store: JobStore,
    executions: Arc<AtomicUsize>,
    resumes: Arc<AtomicUsize>,
}

impl ControllerJobDriver for CompletedDriver {
    fn job_type(&self) -> &'static str {
        "test.15327-completion-worker"
    }
    fn version(&self) -> u32 {
        1
    }
    fn public_request(&self, request: &Value) -> Result<Value> {
        Ok(request.clone())
    }
    fn public_progress(&self, progress: &Value) -> Result<Value> {
        Ok(progress.clone())
    }
    fn public_result(&self, result: &Value) -> Result<Value> {
        Ok(result.clone())
    }
    fn public_error(&self, error: &Error) -> ControllerJobPublicError {
        ControllerJobPublicError {
            message: error.to_string(),
            data: json!({"error_code": error.code.as_str()}),
        }
    }
    fn validate_secret_references(&self, _: &Value) -> Result<()> {
        Ok(())
    }
    fn execute(&self, _: Value, _: ControllerJobHandle) -> Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        self.store.fail_next_durable_writes(1);
        Ok(json!({"finished_side_effect": true, "payload": "x".repeat(8192)}))
    }
    fn resume(&self, checkpoint: Value, job: ControllerJobHandle) -> Result<Value> {
        self.resumes.fetch_add(1, Ordering::SeqCst);
        self.execute(checkpoint, job)
    }
    fn cancel(&self, _: &Value) -> Result<()> {
        Ok(())
    }
}

#[test]
fn controller_completion_soak_releases_workers_and_never_resumes_finished_side_effects() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation_with_retention_and_terminal_byte_limit(
        &path,
        16,
        4,
        64 * 1024,
    )
    .unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let resumes = Arc::new(AtomicUsize::new(0));
    let driver = Arc::new(CompletedDriver {
        store: store.clone(),
        executions: executions.clone(),
        resumes: resumes.clone(),
    });
    controller_job_driver::register_controller_job_driver(driver).unwrap();
    #[cfg(target_os = "linux")]
    let initial_rss = rss_bytes();
    for _ in 0..50 {
        let id = running_controller(&store, "test.15327-completion-worker");
        let state = store.controller_job_state(id).unwrap();
        let key = controller_job_runtime_key(&store, id);
        dispatch_claimed_controller_job(store.clone(), id, state, false);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let pending = store.controller_completion_pending(id).unwrap();
            let owned = controller_job_runtimes().lock().unwrap().contains_key(&key);
            if pending && !owned {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "completed driver must hand custody off and release its worker"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            store.get(id).unwrap().status,
            JobStatus::Running,
            "failed snapshot write does not publish premature success"
        );
        recover_controller_jobs(&store);
        assert_eq!(store.get(id).unwrap().status, JobStatus::Succeeded);
        assert!(!store.controller_completion_pending(id).unwrap());
        assert!(!controller_job_runtimes().lock().unwrap().contains_key(&key));
        let before = fs::read(&path).unwrap();
        for _ in 0..16 {
            assert!(persist_controller_success_or_uncertainty(
                &store.handle(id),
                json!({"duplicate": true})
            ));
        }
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(
            before.len() < 80 * 1024,
            "primary queue stays bounded independently of completed history"
        );
    }
    let restarted = JobStore::open_without_reconciliation(&path).unwrap();
    recover_controller_jobs(&restarted);
    assert_eq!(executions.load(Ordering::SeqCst), 50);
    assert_eq!(resumes.load(Ordering::SeqCst), 0);
    assert!(restarted.active_controller_jobs().is_empty());
    let report = JobStore::retained_owner_report_at_path(&path).unwrap();
    assert!(report["jobs"]["count"].as_u64().unwrap() <= 4);
    assert_eq!(report["controller_completion_ledger"]["count"], 0);
    #[cfg(target_os = "linux")]
    {
        let growth = rss_bytes().saturating_sub(initial_rss);
        eprintln!("15327 soak: 50 completions, 800 duplicate observations; RSS growth {growth} bytes; queue bytes {}", fs::metadata(&path).unwrap().len());
        assert!(
            growth < 64 * 1024 * 1024,
            "bounded completion soak cannot retain historical snapshots"
        );
    }
}

#[cfg(target_os = "linux")]
fn rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmRSS:")
                .and_then(|value| value.split_whitespace().next()?.parse::<u64>().ok())
        })
        .expect("Linux VmRSS")
        * 1024
}
