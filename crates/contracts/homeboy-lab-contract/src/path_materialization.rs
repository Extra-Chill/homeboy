//! Lab policy layered on canonical runner path materialization types.

pub use homeboy_runner_contract::path_materialization::*;

pub const PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT: &str = "lab.execution_context";
pub const PATH_MATERIALIZATION_OWNER_LAB_PROVIDER_CONFIG: &str = "lab.provider_config";

pub fn primary_workspace_existing_remote(
    remote_path: impl Into<String>,
) -> PathMaterializationEntry {
    PathMaterializationEntry::new(
        PATH_MATERIALIZATION_ROLE_PRIMARY_WORKSPACE,
        PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT,
        None,
        remote_path,
        PATH_MATERIALIZATION_MODE_EXISTING_REMOTE,
        PATH_MATERIALIZATION_STATUS_VALIDATED,
    )
}
