pub(super) const RUNNER_STEPS_SH: &str = include_str!("../runtime/runner-steps.sh");
pub(super) const RUNNER_PRELUDE_SH: &str = include_str!("../runtime/runner-prelude.sh");
pub(super) const COMMAND_CAPTURE_SH: &str = include_str!("../runtime/command-capture.sh");
pub(super) const BASH_PREFLIGHT_SH: &str = include_str!("../runtime/bash-preflight.sh");
pub(super) const FAILURE_TRAP_SH: &str = include_str!("../runtime/failure-trap.sh");
pub(super) const WRITE_TEST_RESULTS_SH: &str = include_str!("../runtime/write-test-results.sh");
pub(super) const SIDECAR_WRITER_SH: &str = include_str!("../runtime/sidecar-writer.sh");
pub(super) const RESOLVE_CONTEXT_SH: &str = include_str!("../runtime/resolve-context.sh");
pub(super) const BENCH_HELPER_SH: &str = include_str!("../runtime/bench-helper.sh");
pub(super) const BENCH_HELPER_JS: &str = include_str!("../runtime/bench-helper.mjs");
pub(super) const BENCH_HELPER_PHP: &str = include_str!("../runtime/bench-helper.php");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_runtime_helpers_are_present() {
        for content in [
            RUNNER_STEPS_SH,
            RUNNER_PRELUDE_SH,
            COMMAND_CAPTURE_SH,
            BASH_PREFLIGHT_SH,
            FAILURE_TRAP_SH,
            WRITE_TEST_RESULTS_SH,
            SIDECAR_WRITER_SH,
            RESOLVE_CONTEXT_SH,
            BENCH_HELPER_SH,
            BENCH_HELPER_JS,
            BENCH_HELPER_PHP,
        ] {
            assert!(!content.trim().is_empty());
        }
    }
}
