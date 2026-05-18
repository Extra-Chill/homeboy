use std::path::Path;

use crate::core::component;
use crate::core::error::Result;

pub fn infer_attached_component_id(local_path: &Path) -> Result<String> {
    component::infer_portable_component_id(local_path)
}

pub fn discover_attached_component(local_path: &Path) -> Option<component::Component> {
    component::discover_from_portable(local_path)
}
