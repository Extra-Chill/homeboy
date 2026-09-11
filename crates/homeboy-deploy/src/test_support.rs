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
