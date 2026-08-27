//! External check detail resolver fixtures.
//!
//! Both tests require the `test-support` feature: the first calls a fixture
//! entry point that only exists under it, and the second is not a test anyone
//! runs directly — `timeout_fixture_command` copies this binary and re-invokes
//! it with `--exact resolver_fixture_hangs_until_killed` to get a process that
//! hangs until the resolver's timeout kills it. So the two must stay in the
//! same binary, and gating the file keeps them together.
//!
//! Without the gate this target failed to compile whenever the feature was
//! absent, which broke a bare `cargo test -p homeboy-cli` (#13624). CI always
//! passes `--features test-support`, so it never saw it.
#![cfg(feature = "test-support")]

#[test]
fn resolver_fixture_is_bounded_and_cross_platform() {
    homeboy_cli::commands::ci::test_external_check_detail_resolver_fixture();
}

#[test]
#[ignore = "invoked by the external resolver timeout fixture"]
fn resolver_fixture_hangs_until_killed() {
    std::thread::sleep(std::time::Duration::from_secs(30));
}
