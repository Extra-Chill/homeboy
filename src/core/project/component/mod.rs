pub mod attachments;
pub mod discovery;
pub mod overrides;
pub mod report;
pub mod resolution;

pub use attachments::{
    attach_component_path, attach_discovered_component_path, clear_component_attachments,
    has_component, project_component_ids, remove_components, set_component_attachments,
};
pub use overrides::apply_component_overrides;
pub use report::{
    attach_component_path_report, clear_components, list_components, remove_components_report,
    set_components, ProjectComponentsOutput,
};
pub use resolution::{resolve_project_component, resolve_project_components};
