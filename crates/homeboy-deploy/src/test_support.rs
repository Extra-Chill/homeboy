//! Shared deploy test fixtures.

use homeboy_core::server::SshClient;
use std::collections::HashMap;

/// A local `SshClient` fixture.
///
/// Deploy tests that exercise local execution all need the same client, so it
/// is built once here rather than restated per module.
pub(crate) fn local_client() -> SshClient {
    SshClient {
        host: "localhost".to_string(),
        user: "test".to_string(),
        port: 22,
        identity_file: None,
        auth: None,
        is_local: true,
        env: HashMap::new(),
    }
}

/// Offline stand-ins for the `gh` and `curl` binaries that GitHub release
/// lookups shell out to.
///
/// Checkout-less components resolve release tags through the GitHub API, so a
/// test that plans one would otherwise reach the network. This guard prepends
/// fake binaries to `PATH`: `gh` reports no token, and `curl` either answers
/// HTTP 404 for every request or fails like an unreachable host. With no token,
/// metadata lookup does not pipe a curl config, so the stub must not read the
/// inherited stdin (which may be open for the lifetime of the test runner).
/// Callers must hold
/// [`homeboy_core::test_support::env_lock`] (or be inside `with_isolated_home`)
/// while the guard lives.
#[cfg(unix)]
pub(crate) struct OfflineGithub {
    _path: homeboy_core::test_support::EnvVarGuard,
    _bin: tempfile::TempDir,
}

#[cfg(unix)]
impl OfflineGithub {
    /// Every GitHub request answers HTTP 404, i.e. no such release exists.
    pub(crate) fn not_found() -> Self {
        Self::with_curl("#!/bin/sh\nprintf '\\n404'\n")
    }

    /// Every GitHub request fails the way curl does when the host is unreachable.
    pub(crate) fn unreachable() -> Self {
        Self::with_curl("#!/bin/sh\necho 'curl: (7) Failed to connect' >&2\nexit 7\n")
    }

    fn with_curl(curl_script: &str) -> Self {
        let bin = tempfile::tempdir().expect("fake GitHub bin dir");
        homeboy_test_support::write_executable(
            &bin.path().join("gh"),
            "#!/bin/sh\nexit 1\n",
            0o755,
        )
        .expect("write fake gh");
        homeboy_test_support::write_executable(&bin.path().join("curl"), curl_script, 0o755)
            .expect("write fake curl");
        let mut path = bin.path().as_os_str().to_os_string();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let _path = homeboy_core::test_support::EnvVarGuard::set("PATH", path);
        Self { _path, _bin: bin }
    }
}
