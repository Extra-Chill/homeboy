use chrono::Local;
use serde::Serialize;
use std::path::Path;

use crate::component;
use crate::core::release::version;
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use crate::paths::resolve_path;

use super::io::*;
use super::sections::*;
use super::settings::*;

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

// === Changelog Init Operations ===

#[derive(Debug, Clone, Serialize)]
pub struct InitOutput {
    pub component_id: String,
    pub changelog_path: String,
    pub initial_version: String,
    pub next_section_label: String,
    pub created: bool,
    pub changed: bool,
    pub configured: bool,
}

fn generate_template(initial_version: &str, next_label: &str) -> String {
    let today = Local::now().format("%Y-%m-%d");
    format!(
        "# Changelog\n\n## {}\n\n## [{}] - {}\n- Initial release\n",
        next_label, initial_version, today
    )
}

/// Initialize a changelog for a component.
/// If the changelog file doesn't exist, creates a new one with Keep a Changelog template.
/// If the changelog file exists, ensures it has an Unreleased section.
pub fn init(component_id: &str, path: Option<&str>, configure: bool) -> Result<InitOutput> {
    let component = component::resolve_effective(Some(component_id), None, None)?;

    // Validate local_path is absolute and exists before any file operations
    component::validate_local_path(&component)?;

    let settings = resolve_effective_settings(Some(&component));

    // Determine changelog path (relative to component)
    let mut relative_path = path.unwrap_or("CHANGELOG.md").to_string();
    let mut changelog_path = resolve_path(&component.local_path, &relative_path);

    // Check for existing changelog_target configuration
    if let Some(ref configured_target) = component.changelog_target {
        let configured_path = resolve_path(&component.local_path, configured_target);

        // If user didn't specify a custom path, or specified the same path, check for existing changelog
        if (path.is_none() || path == Some(configured_target)) && configured_path.exists() {
            return Err(Error::validation_invalid_argument(
                "changelog",
                "Changelog already exists for this component",
                None,
                Some(vec![
                    format!("Existing changelog at: {}", configured_path.display()),
                    format!("View with: homeboy changelog show {}", component_id),
                    format!("Or use --path to specify a different location"),
                ]),
            ));
        }
    } else {
        // No changelog_target configured - scan for common changelog filenames
        let changelog_candidates = [
            "CHANGELOG.md",
            "changelog.md",
            "docs/CHANGELOG.md",
            "docs/changelog.md",
            "HISTORY.md",
        ];

        let local_path = Path::new(&component.local_path);
        for candidate in &changelog_candidates {
            let candidate_path = local_path.join(candidate);
            if candidate_path.exists() {
                if configure {
                    // User wants to configure existing changelog - update the path and continue
                    relative_path = candidate.to_string();
                    changelog_path = candidate_path;
                    break;
                }
                return Err(Error::validation_invalid_argument(
                    "changelog",
                    "Found existing changelog file",
                    None,
                    Some(vec![
                        format!("Existing changelog at: {}", candidate_path.display()),
                        format!("Configure and use it: homeboy changelog init {} --path \"{}\" --configure", component_id, candidate),
                        format!("View with: homeboy changelog show {}", component_id),
                    ]),
                ));
            }
        }
    }

    // Configure component if requested (do this regardless of file state)
    let configured = if configure {
        component::set_changelog_target(component_id, &relative_path)?;
        true
    } else {
        false
    };

    // Handle existing file: ensure Unreleased section exists
    if changelog_path.exists() {
        let content = local_files::read_file(&changelog_path, "read changelog")?;

        let (new_content, changed) = ensure_next_section(&content, &settings.next_section_aliases)?;

        if changed {
            local_files::local().write(&changelog_path, &new_content)?;
        }

        return Ok(InitOutput {
            component_id: component_id.to_string(),
            changelog_path: changelog_path.to_string_lossy().to_string(),
            initial_version: String::new(),
            next_section_label: settings.next_section_label,
            created: false,
            changed,
            configured,
        });
    }

    // File doesn't exist: create new changelog with template
    let version_info = version::read_version(Some(component_id))?;
    let initial_version = version_info.version;

    if let Some(parent) = changelog_path.parent() {
        local_files::local().ensure_dir(parent)?;
    }

    let content = generate_template(&initial_version, &settings.next_section_label);
    local_files::local().write(&changelog_path, &content)?;

    Ok(InitOutput {
        component_id: component_id.to_string(),
        changelog_path: changelog_path.to_string_lossy().to_string(),
        initial_version,
        next_section_label: settings.next_section_label,
        created: true,
        changed: true,
        configured,
    })
}
