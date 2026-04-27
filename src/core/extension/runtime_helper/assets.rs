pub(super) const RUNNER_STEPS_SH: &str = include_str!("../runtime/runner-steps.sh");
pub(super) const FAILURE_TRAP_SH: &str = include_str!("../runtime/failure-trap.sh");
pub(super) const WRITE_TEST_RESULTS_SH: &str = include_str!("../runtime/write-test-results.sh");
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
            FAILURE_TRAP_SH,
            WRITE_TEST_RESULTS_SH,
            RESOLVE_CONTEXT_SH,
            BENCH_HELPER_SH,
            BENCH_HELPER_JS,
            BENCH_HELPER_PHP,
        ] {
            assert!(!content.trim().is_empty());
        }
    }
}
