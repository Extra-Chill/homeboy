//! Cross-process runner execution-context vocabulary.

/// Set while a hosted exec runs inside a runner rather than the local host.
pub const RUNNER_HOSTED_EXEC_ENV: &str = "HOMEBOY_RUNNER_HOSTED_EXEC";

/// Private marker added only when an exec crosses a remote runner boundary.
pub const RUNNER_PLACEMENT_RESOLVED_ENV: &str = "HOMEBOY_RUNNER_PLACEMENT_RESOLVED";

/// Identifies the runner an exec is bound to.
pub const RUNNER_ID_ENV: &str = "HOMEBOY_RUNNER_ID";

/// Whether an environment variable is a private runner control marker.
pub fn is_internal_control_env(name: &str) -> bool {
    name == RUNNER_PLACEMENT_RESOLVED_ENV
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_context_vocabulary_and_classification_stay_stable() {
        assert_eq!(RUNNER_HOSTED_EXEC_ENV, "HOMEBOY_RUNNER_HOSTED_EXEC");
        assert_eq!(
            RUNNER_PLACEMENT_RESOLVED_ENV,
            "HOMEBOY_RUNNER_PLACEMENT_RESOLVED"
        );
        assert_eq!(RUNNER_ID_ENV, "HOMEBOY_RUNNER_ID");

        assert!(is_internal_control_env(RUNNER_PLACEMENT_RESOLVED_ENV));
        assert!(!is_internal_control_env(RUNNER_HOSTED_EXEC_ENV));
        assert!(!is_internal_control_env(RUNNER_ID_ENV));
        assert!(!is_internal_control_env("HOMEBOY_LAB_EXECUTION_RUNNER_ID"));
    }
}
