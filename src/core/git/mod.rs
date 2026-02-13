mod primitives;
mod commits;
mod operations;
mod changes;

pub use primitives::*;
pub use commits::*;
pub use operations::*;
pub use changes::*;

use std::process::Command;

use crate::error::Error;

fn execute_git(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
}

fn resolve_target(component_id: Option<&str>) -> crate::error::Result<(String, String)> {
    let id = component_id.ok_or_else(|| {
        Error::validation_invalid_argument(
            "componentId",
            "Missing componentId",
            None,
            Some(vec![
                "Provide a component ID: homeboy git <command> <component-id>".to_string(),
                "List available components: homeboy component list".to_string(),
            ]),
        )
    })?;
    let comp = crate::component::load(id)?;
    Ok((id.to_string(), comp.local_path))
}
