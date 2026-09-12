//! Shared fixtures for `homeboy runs` tests.

use homeboy::core::observation::NewRunRecord;
use serde_json::Value;

/// A run record fixture with fixed provenance.
///
/// Eight `runs` submodules needed the same record, so it is built once here
/// rather than restated per file.
pub(super) fn sample_run(
    kind: &str,
    component_id: &str,
    rig_id: &str,
    metadata: Value,
) -> NewRunRecord {
    NewRunRecord::builder(kind)
        .component_id(component_id)
        .command(format!("homeboy {kind} {component_id}"))
        .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
        .homeboy_version("test-version")
        .git_sha(Some("abc123".to_string()))
        .rig_id(rig_id)
        .metadata(metadata)
        .build()
}
