#[test]
fn resolver_fixture_is_bounded_and_cross_platform() {
    homeboy_cli::commands::ci::test_external_check_detail_resolver_fixture();
}

#[test]
#[ignore = "invoked by the external resolver timeout fixture"]
fn resolver_fixture_hangs_until_killed() {
    std::thread::sleep(std::time::Duration::from_secs(30));
}
