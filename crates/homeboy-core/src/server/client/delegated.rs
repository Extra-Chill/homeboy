use std::time::Duration;

pub(crate) const DELEGATED_RUN_STATUS_FILE_ENV: &str = "HOMEBOY_DELEGATED_RUN_STATUS_FILE";
pub(crate) const DELEGATED_RUN_STATUS_POINTER_ENV: &str = "HOMEBOY_DELEGATED_RUN_STATUS_POINTER";
pub(crate) const DELEGATED_RUN_ERROR_POINTER_ENV: &str = "HOMEBOY_DELEGATED_RUN_ERROR_POINTER";
pub(crate) const DELEGATED_RUN_POLL_MS_ENV: &str = "HOMEBOY_DELEGATED_RUN_POLL_MS";

#[derive(Debug, Clone)]
pub(crate) struct DelegatedRunFailureMonitor {
    status_file: String,
    status_pointer: String,
    error_pointer: Option<String>,
    pub(crate) poll_interval: Duration,
}

#[derive(Debug, Clone)]
pub(crate) struct DelegatedRunTerminalFailure {
    status: String,
    detail: Option<String>,
    status_file: String,
}

impl DelegatedRunFailureMonitor {
    pub(crate) fn from_env(env: Option<&[(&str, &str)]>) -> Option<Self> {
        let status_file = env_value(env, DELEGATED_RUN_STATUS_FILE_ENV)?;
        let status_pointer = env_value(env, DELEGATED_RUN_STATUS_POINTER_ENV)
            .unwrap_or_else(|| "/status".to_string());
        let error_pointer = env_value(env, DELEGATED_RUN_ERROR_POINTER_ENV);
        let poll_interval = env_value(env, DELEGATED_RUN_POLL_MS_ENV)
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_millis(250));

        Some(Self {
            status_file,
            status_pointer,
            error_pointer,
            poll_interval,
        })
    }

    pub(crate) fn terminal_failure(&self) -> Option<DelegatedRunTerminalFailure> {
        let content = std::fs::read_to_string(&self.status_file).ok()?;
        let value = serde_json::from_str::<serde_json::Value>(&content).ok()?;
        let status = value
            .pointer(&self.status_pointer)
            .and_then(serde_json::Value::as_str)?
            .trim()
            .to_ascii_lowercase();

        if !delegated_status_is_terminal_failure(&status) {
            return None;
        }

        Some(DelegatedRunTerminalFailure {
            status,
            detail: self.failure_detail(&value),
            status_file: self.status_file.clone(),
        })
    }

    fn failure_detail(&self, value: &serde_json::Value) -> Option<String> {
        if let Some(pointer) = &self.error_pointer {
            if let Some(detail) = json_pointer_string(value, pointer) {
                return Some(detail);
            }
        }

        ["/error", "/message", "/summary", "/failure/message"]
            .iter()
            .find_map(|pointer| json_pointer_string(value, pointer))
    }
}

impl DelegatedRunTerminalFailure {
    fn stderr_message(&self) -> String {
        let mut message = format!(
            "Delegated runtime reached terminal failure status `{}` from {}. Homeboy terminated the wrapper process group and returned failure evidence.",
            self.status, self.status_file
        );
        if let Some(detail) = &self.detail {
            message.push_str("\nDelegated runtime detail: ");
            message.push_str(detail);
        }
        message
    }
}

fn env_value(env: Option<&[(&str, &str)]>, name: &str) -> Option<String> {
    env.and_then(|pairs| {
        pairs
            .iter()
            .find_map(|(key, value)| (*key == name).then(|| (*value).to_string()))
    })
    .or_else(|| std::env::var(name).ok())
    .filter(|value| !value.trim().is_empty())
}

fn delegated_status_is_terminal_failure(status: &str) -> bool {
    matches!(
        status,
        "failed"
            | "failure"
            | "error"
            | "errored"
            | "canceled"
            | "cancelled"
            | "timeout"
            | "timed_out"
    )
}

fn json_pointer_string(value: &serde_json::Value, pointer: &str) -> Option<String> {
    match value.pointer(pointer)? {
        serde_json::Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        other if !other.is_null() => Some(other.to_string()),
        _ => None,
    }
}

pub(crate) fn stderr_with_delegated_failure(
    mut stderr: String,
    failure: Option<&DelegatedRunTerminalFailure>,
) -> String {
    let Some(failure) = failure else {
        return stderr;
    };
    if !stderr.is_empty() && !stderr.ends_with('\n') {
        stderr.push('\n');
    }
    stderr.push_str(&failure.stderr_message());
    stderr
}
