//! Ambient runner-job identity and control state that must not leak into gates.

/// Prefixes and exact names owned by the runner job, not by a gate invocation.
/// Capability/tool configuration (for example `HOMEBOY_WP_CODEBOX_BIN`) is not
/// identity and deliberately remains outside this contract.
pub const RUNNER_JOB_IDENTITY_ENV: (&[&str], &[&str]) = (
    &[
        "HOMEBOY_RUNNER_",
        "HOMEBOY_LAB_EXECUTION_",
        "HOMEBOY_DAEMON_",
    ],
    &[
        crate::runner_job_execution_context::RUNNER_JOB_ID_ENV,
        crate::runner_job_execution_context::RUNNER_CHILD_RESERVATION_ENV,
        crate::runner_job_execution_context::RUNNER_JOB_EXECUTION_CONTEXT_ID_ENV,
        crate::runner_job_execution_context::DAEMON_CHILD_BUILD_IDENTITY_ENV,
        "HOMEBOY_RUN_ID",
        crate::observation::ACTIVE_RUN_ID_ENV,
        "HOMEBOY_BENCH_RUN_ID",
        "HOMEBOY_COMMAND",
        "HOMEBOY_SETTINGS_JSON",
        crate::observation::context::SOURCE_SNAPSHOT_METADATA_ENV,
    ],
);

/// Whether an environment name belongs to the ambient runner job.
pub fn is_runner_job_identity_env(name: &str) -> bool {
    let (prefixes, names) = RUNNER_JOB_IDENTITY_ENV;
    prefixes.iter().any(|prefix| name.starts_with(prefix)) || names.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_names_do_not_include_gate_capabilities() {
        let (prefixes, names) = RUNNER_JOB_IDENTITY_ENV;
        for prefix in prefixes {
            assert!(is_runner_job_identity_env(&format!(
                "{prefix}FUTURE_CONTROL"
            )));
        }
        for name in names {
            assert!(is_runner_job_identity_env(name));
        }
        for name in [
            "HOMEBOY_WP_CODEBOX_BIN",
            "HOMEBOY_WP_CODEBOX_CORE_MODULE",
            "HOMEBOY_WP_CODEBOX_INSTALL_DIR",
            "HOMEBOY_NO_UPDATE_CHECK",
            "HOMEBOY_CONFIG_ROOT",
            "HOMEBOY_DATA_DIR",
            "HOMEBOY_RUN_ID_SUFFIX",
        ] {
            assert!(!is_runner_job_identity_env(name), "{name}");
        }
    }
}
