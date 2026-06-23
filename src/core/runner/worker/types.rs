use serde::Serialize;

use crate::core::api_jobs::Job;

#[derive(Debug, Clone)]
pub struct ReverseRunnerWorkerOptions {
    pub runner_id: String,
    pub broker_url: String,
    /// Paired broker bearer token. Required when the broker enforces auth;
    /// omitted for loopback-open smoke setups.
    pub broker_token: Option<String>,
    pub project_id: Option<String>,
    pub lease_ms: u64,
    pub concurrency_limit: Option<usize>,
    pub loop_mode: bool,
    pub idle_backoff_ms: u64,
    pub max_idle_backoff_ms: u64,
    pub broker_failure_backoff_ms: u64,
    pub broker_retry_limit: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReverseRunnerWorkerOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub broker_url: String,
    pub claimed: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub loop_mode: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub iterations: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub jobs_claimed: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub broker_failures: u32,
    #[serde(skip_serializing_if = "is_false")]
    pub stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_claim: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<Job>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero<T>(value: &T) -> bool
where
    T: PartialEq + From<u8>,
{
    *value == T::from(0)
}
