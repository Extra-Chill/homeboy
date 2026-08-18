pub mod attachments;
pub mod overrides;
pub mod report;
pub mod resolution;

pub use attachments::{
    attach_component_path, attach_discovered_component_path, clear_component_attachments,
    has_component, project_component_ids, rebase_monorepo_component_paths, remove_components,
    set_component_attachments, MonorepoComponentPathChange, MonorepoComponentPathStatus,
};
pub use overrides::apply_component_overrides;
pub use report::{
    attach_component_path_report, attach_component_paths_report, clear_components, list_components,
    remove_components_report, set_components, BatchComponentAttachmentFailurePolicy,
    BatchComponentAttachmentInput, BatchComponentAttachmentWorktreePolicy, ProjectComponentsOutput,
};
pub use resolution::{
    bind_materialized_component_at_path, bind_materialized_component_at_path_in_root,
    bind_materialized_component_to_project, bind_materialized_component_to_project_in_root,
    resolve_project_component, resolve_project_component_in_root,
    resolve_project_component_with_standalone_snapshot,
    resolve_project_component_with_standalone_snapshot_in_root, resolve_project_components,
    resolve_project_components_in_root, StandaloneComponentConfigSnapshot,
};
