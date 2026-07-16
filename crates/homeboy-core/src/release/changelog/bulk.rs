use serde::Serialize;

use crate::component;
use crate::engine::local_files;
use crate::error::Result;

use super::io::*;

// === Changelog Show Operations ===

#[derive(Debug, Clone, Serialize)]
pub struct ShowOutput {
    pub component_id: String,
    pub changelog_path: String,
    pub content: String,
}

pub fn show(component_id: &str) -> Result<ShowOutput> {
    let component = component::resolve_effective(Some(component_id), None, None)?;
    let changelog_path = resolve_changelog_path(&component)?;

    let content = local_files::read_file(
        &changelog_path,
        &format!("read changelog at {}", changelog_path.display()),
    )?;

    Ok(ShowOutput {
        component_id: component_id.to_string(),
        changelog_path: changelog_path.to_string_lossy().to_string(),
        content,
    })
}
