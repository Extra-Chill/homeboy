mod result_tests;
mod support;
mod worker_tests;

use super::types::ReverseRunnerWorkerOptions;

fn worker_options(broker_url: String) -> ReverseRunnerWorkerOptions {
    ReverseRunnerWorkerOptions {
        runner_id: "lab".to_string(),
        broker_url,
        broker_token: None,
        project_id: None,
        lease_ms: 30_000,
        concurrency_limit: None,
        loop_mode: false,
        idle_backoff_ms: 1,
        max_idle_backoff_ms: 10,
        broker_failure_backoff_ms: 1,
        broker_retry_limit: 1,
    }
}
