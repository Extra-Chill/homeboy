use std::path::Path;

use crate::component;
use crate::error::Result;

pub fn infer_attached_component_id(local_path: &Path) -> Result<String> {
    component::infer_portable_component_id(local_path)
}

pub fn discover_attached_component(local_path: &Path) -> Option<component::Component> {
    component::discover_from_portable(local_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_infer_attached_component_id_default_path() {
        let local_path = Path::new("");
        let _result = infer_attached_component_id(&local_path);
    }

    #[test]
    fn test_discover_attached_component_default_path() {
        let local_path = Path::new("");
        let _result = discover_attached_component(&local_path);
    }

}
