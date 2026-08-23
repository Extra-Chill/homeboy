#[test]
fn discovery_invokes_cross_platform_fixture_for_success_unavailable_and_malformed() {
    homeboy_cli::commands::ci::test_external_check_detail_resolver_fixture();
}

#[cfg(target_os = "linux")]
#[test]
fn inherited_pipe_holder_is_reaped_before_capture_snapshot() {
    homeboy_cli::commands::ci::test_external_check_detail_resolver_pipe_holder_cleanup();
}
