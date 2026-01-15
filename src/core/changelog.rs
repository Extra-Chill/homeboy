use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::component::{self, Component};
use crate::config::read_json_spec_to_string;
use crate::core::local_files::{self, FileSystem};
use crate::core::version;
use crate::error::{Error, Result};
use crate::project;

const DEFAULT_NEXT_SECTION_LABEL: &str = "Unreleased";

#[derive(Debug, Clone)]
pub struct EffectiveChangelogSettings {
    pub next_section_label: String,
    pub next_section_aliases: Vec<String>,
}

pub fn resolve_effective_settings(component: Option<&Component>) -> EffectiveChangelogSettings {
    let project_settings = component
        .and_then(|c| component::projects_using(&c.id).ok())
        .and_then(|projects| {
            if projects.len() == 1 {
                project::load(&projects[0]).ok()
            } else {
                None
            }
        });

    let next_section_label = component
        .and_then(|c| c.changelog_next_section_label.clone())
        .or_else(|| {
            project_settings
                .as_ref()
                .and_then(|p| p.changelog_next_section_label.clone())
        })
        .unwrap_or_else(|| DEFAULT_NEXT_SECTION_LABEL.to_string());

    let mut next_section_aliases = component
        .and_then(|c| c.changelog_next_section_aliases.clone())
        .or_else(|| project_settings.and_then(|p| p.changelog_next_section_aliases.clone()))
        .unwrap_or_default();

    if next_section_aliases.is_empty() {
        next_section_aliases.extend([
            next_section_label.clone(),
            format!("[{}]", next_section_label),
        ]);
    } else {
        let label_alias = next_section_label.trim();
        let bracketed_alias = format!("[{}]", label_alias);

        let mut has_label = false;
        let mut has_bracketed = false;

        for alias in &next_section_aliases {
            let trimmed_alias = alias.trim();
            if trimmed_alias == label_alias {
                has_label = true;
            }
            if trimmed_alias == bracketed_alias {
                has_bracketed = true;
            }
        }

        if !has_label {
            next_section_aliases.push(next_section_label.clone());
        }
        if !has_bracketed {
            next_section_aliases.push(format!("[{}]", next_section_label));
        }
    }

    EffectiveChangelogSettings {
        next_section_label,
        next_section_aliases,
    }
}

pub fn resolve_changelog_path(component: &Component) -> Result<PathBuf> {
    // If explicitly configured, use that
    if let Some(target) = component.changelog_target.as_ref() {
        return resolve_target_path(&component.local_path, target);
    }

    // Auto-detect from well-known locations
    if let Some(path) = detect_changelog_path(&component.local_path) {
        return Ok(path);
    }

    // No changelog found - provide helpful error
    Err(Error::validation_invalid_argument(
        "component.changelog_target",
        "No changelog found for component",
        None,
        Some(vec![
            format!(
                "Configure existing: homeboy component set {} --changelog-target \"CHANGELOG.md\"",
                component.id
            ),
            format!(
                "Create new: homeboy changelog init {} --configure",
                component.id
            ),
            "Bypass changelog: homeboy version set <version>".to_string(),
        ]),
    ))
}

fn resolve_target_path(local_path: &str, file: &str) -> Result<PathBuf> {
    let path = if file.starts_with('/') {
        PathBuf::from(file)
    } else {
        Path::new(local_path).join(file)
    };

    Ok(path)
}

pub fn add_next_section_item(
    changelog_content: &str,
    next_section_aliases: &[String],
    message: &str,
) -> Result<(String, bool)> {
    let trimmed_message = message.trim();
    if trimmed_message.is_empty() {
        return Err(Error::validation_invalid_argument(
            "message",
            "Changelog message cannot be empty",
            None,
            None,
        ));
    }

    let (with_section, section_changed) =
        ensure_next_section(changelog_content, next_section_aliases)?;
    let (with_item, item_changed) =
        append_item_to_next_section(&with_section, next_section_aliases, trimmed_message)?;

    Ok((with_item, section_changed || item_changed))
}

pub fn add_next_section_items(
    changelog_content: &str,
    next_section_aliases: &[String],
    messages: &[String],
) -> Result<(String, bool, usize)> {
    if messages.is_empty() {
        return Err(Error::validation_invalid_argument(
            "messages",
            "Changelog messages cannot be empty",
            None,
            None,
        ));
    }

    let (mut content, mut changed) = ensure_next_section(changelog_content, next_section_aliases)?;
    let mut items_added = 0;

    for message in messages {
        let trimmed_message = message.trim();
        if trimmed_message.is_empty() {
            return Err(Error::validation_invalid_argument(
                "messages",
                "Changelog messages cannot include empty values",
                None,
                None,
            ));
        }

        let (next, item_changed) =
            append_item_to_next_section(&content, next_section_aliases, trimmed_message)?;
        if item_changed {
            items_added += 1;
            changed = true;
        }
        content = next;
    }

    Ok((content, changed, items_added))
}

pub fn read_and_add_next_section_item(
    component: &Component,
    settings: &EffectiveChangelogSettings,
    message: &str,
) -> Result<(PathBuf, bool)> {
    let path = resolve_changelog_path(component)?;
    let content = fs::read_to_string(&path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read changelog".to_string())))?;

    let (new_content, changed) =
        add_next_section_item(&content, &settings.next_section_aliases, message)?;

    if changed {
        fs::write(&path, new_content)
            .map_err(|e| Error::internal_io(e.to_string(), Some("write changelog".to_string())))?;
    }

    Ok((path, changed))
}

pub fn read_and_add_next_section_items(
    component: &Component,
    settings: &EffectiveChangelogSettings,
    messages: &[String],
) -> Result<(PathBuf, bool, usize)> {
    let path = resolve_changelog_path(component)?;
    let content = fs::read_to_string(&path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read changelog".to_string())))?;

    let (new_content, changed, items_added) =
        add_next_section_items(&content, &settings.next_section_aliases, messages)?;

    if changed {
        fs::write(&path, new_content)
            .map_err(|e| Error::internal_io(e.to_string(), Some("write changelog".to_string())))?;
    }

    Ok((path, changed, items_added))
}

pub fn finalize_next_section(
    changelog_content: &str,
    next_section_aliases: &[String],
    new_version: &str,
    allow_empty: bool,
) -> Result<(String, bool)> {
    if new_version.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "newVersion",
            "New version label cannot be empty",
            None,
            None,
        ));
    }

    let lines: Vec<&str> = changelog_content.lines().collect();
    let start = find_next_section_start(&lines, next_section_aliases).ok_or_else(|| {
        Error::validation_invalid_argument(
            "changelog",
            "Next changelog section not found (cannot finalize)",
            None,
            Some(vec![
                "Add a next section heading like '## Unreleased' (or configure changelogNextSectionLabel/aliases).".to_string(),
                "Run `homeboy changelog init <component_id>` to add an Unreleased section or create a Keep a Changelog template.".to_string(),
            ]),
        )
    })?;

    let end = find_section_end(&lines, start);
    let body_lines = &lines[start + 1..end];
    let has_content = body_lines.iter().any(|line| !line.trim().is_empty());

    if !has_content {
        if allow_empty {
            return Ok((changelog_content.to_string(), false));
        }

        return Err(Error::validation_invalid_argument(
            "changelog",
            "Next changelog section is empty",
            None,
            Some(vec![
                "Add changelog items before bumping (e.g., `homeboy changelog add <componentId> -m \"...\"`).".to_string(),
                "Ensure the next section contains at least one bullet item.".to_string(),
            ]),
        ));
    }

    // Preserve the exact next-section heading label we found.
    let next_label = lines[start]
        .trim()
        .trim_start_matches('#')
        .trim()
        .to_string();

    let mut out_lines: Vec<String> = Vec::new();

    // Copy everything before ## Unreleased.
    for line in &lines[..start] {
        out_lines.push((*line).to_string());
    }

    // Add new empty ## Unreleased at the top.
    if out_lines.last().is_some_and(|l| !l.trim().is_empty()) {
        out_lines.push(String::new());
    }
    out_lines.push(format!("## {}", next_label));
    out_lines.push(String::new());

    // Replace old ## Unreleased with ## [new_version] - date (Keep a Changelog format).
    let today = Local::now().format("%Y-%m-%d");
    out_lines.push(format!("## [{}] - {}", new_version.trim(), today));
    out_lines.push(String::new());

    // Copy everything after the old heading (body + rest of file).
    // Skip leading blank lines so the new version section starts cleanly.
    let mut started = false;
    for line in &lines[start + 1..] {
        if !started {
            if line.trim().is_empty() {
                continue;
            }
            started = true;
        }
        out_lines.push((*line).to_string());
    }

    // Ensure a blank line between the finalized section and the next heading.
    for idx in 0..out_lines.len().saturating_sub(1) {
        let is_bullet = out_lines[idx].trim_start().starts_with("- ");
        let next_is_heading = out_lines[idx + 1].trim_start().starts_with("## ");

        if is_bullet && next_is_heading {
            out_lines.insert(idx + 1, String::new());
            break;
        }
    }

    let mut out = out_lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }

    Ok((out, true))
}

fn normalize_heading_label(label: &str) -> String {
    label.trim().trim_matches(['[', ']']).trim().to_string()
}

fn is_matching_next_section_heading(line: &str, aliases: &[String]) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with("##") {
        return false;
    }

    let raw_label = trimmed.trim_start_matches('#').trim();
    let normalized = normalize_heading_label(raw_label);

    aliases
        .iter()
        .any(|a| normalize_heading_label(a) == normalized)
}

fn find_next_section_start(lines: &[&str], aliases: &[String]) -> Option<usize> {
    lines
        .iter()
        .position(|line| is_matching_next_section_heading(line, aliases))
}

fn find_section_end(lines: &[&str], start: usize) -> usize {
    let mut index = start + 1;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed.starts_with("##") {
            break;
        }
        index += 1;
    }
    index
}

fn ensure_next_section(content: &str, aliases: &[String]) -> Result<(String, bool)> {
    let lines: Vec<&str> = content.lines().collect();
    if find_next_section_start(&lines, aliases).is_some() {
        return Ok((content.to_string(), false));
    }

    let default_label = aliases.first().map(|s| s.as_str()).unwrap_or("Unreleased");

    // Insert location: after initial "# ..." title block + optional intro paragraph,
    // but before the first version section (## <semver>).
    let mut insert_at = 0usize;

    // Keep a leading title block together.
    while insert_at < lines.len() {
        let line = lines[insert_at];
        if insert_at == 0 && line.trim().starts_with('#') {
            insert_at += 1;
            continue;
        }

        if line.trim().starts_with("##") {
            break;
        }

        insert_at += 1;
    }

    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        if idx == insert_at {
            if !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            if !out.ends_with("\n\n") && !out.is_empty() {
                out.push('\n');
            }
            out.push_str("## ");
            out.push_str(default_label);
            out.push_str("\n\n");
        }
        out.push_str(line);
        out.push('\n');
    }

    if insert_at >= lines.len() {
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str("## ");
        out.push_str(default_label);
        out.push('\n');
    }

    Ok((out, true))
}

/// Extract semver from changelog heading formats:
/// - "0.1.0" -> Some("0.1.0")
/// - "[0.1.0]" -> Some("0.1.0")
/// - "0.1.0 - 2025-01-14" -> Some("0.1.0")
/// - "[0.1.0] - 2025-01-14" -> Some("0.1.0")
/// - "Unreleased" -> None
fn extract_version_from_heading(label: &str) -> Option<String> {
    let semver_pattern = regex::Regex::new(r"\[?(\d+\.\d+\.\d+)\]?").ok()?;
    semver_pattern
        .captures(label)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Get the latest finalized version from the changelog (first ## heading that contains a semver).
/// Supports Keep a Changelog format: `## [X.Y.Z] - YYYY-MM-DD`
/// Returns None if no version section is found.
pub fn get_latest_finalized_version(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            let label = trimmed.trim_start_matches("## ").trim();
            if let Some(version) = extract_version_from_heading(label) {
                return Some(version);
            }
        }
    }
    None
}

fn append_item_to_next_section(
    content: &str,
    aliases: &[String],
    message: &str,
) -> Result<(String, bool)> {
    let lines: Vec<&str> = content.lines().collect();
    let start = find_next_section_start(&lines, aliases).ok_or_else(|| {
        Error::internal_unexpected("Next changelog section not found (unexpected)".to_string())
    })?;

    let section_end = find_section_end(&lines, start);
    let bullet = format!("- {}", message);

    // Check for duplicates
    for line in &lines[start + 1..section_end] {
        if line.trim() == bullet {
            return Ok((content.to_string(), false));
        }
    }

    // Find where to insert: after last bullet, or after blank line following header if no bullets
    let mut insert_after = start;
    let mut has_bullets = false;
    for (i, line) in lines.iter().enumerate().take(section_end).skip(start + 1) {
        if line.trim().starts_with('-') {
            insert_after = i;
            has_bullets = true;
        } else if !has_bullets && line.trim().is_empty() {
            // No bullets yet, insert after the blank line following the header
            insert_after = i;
        }
    }

    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        // Skip trailing blank lines after bullets (we'll add one at the end)
        // Only skip if there are actual bullets
        if has_bullets && idx > insert_after && idx < section_end && lines[idx].trim().is_empty() {
            continue;
        }

        out.push_str(line);
        out.push('\n');

        // Insert new bullet at the insertion point
        if idx == insert_after {
            out.push_str(&bullet);
            out.push('\n');
            // Add blank line after bullets (before next section)
            if section_end < lines.len() {
                out.push('\n');
            }
        }
    }

    Ok((out, true))
}

// === Bulk Operations with JSON Spec ===

#[derive(Debug, Clone, Serialize)]

pub struct AddItemsOutput {
    pub component_id: String,
    pub changelog_path: String,
    pub next_section_label: String,
    pub messages: Vec<String>,
    pub items_added: usize,
    pub changed: bool,
}

#[derive(Debug, Deserialize)]

struct AddItemsInput {
    component_id: String,
    messages: Vec<String>,
}

/// Add changelog items from a JSON spec.
pub fn add_items_bulk(json_spec: &str) -> Result<AddItemsOutput> {
    let raw = read_json_spec_to_string(json_spec)?;

    let input: AddItemsInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(e, Some("parse changelog add input".to_string()))
    })?;

    add_items(Some(&input.component_id), &input.messages)
}

/// Add changelog items to a component. Auto-detects JSON in component_id.
pub fn add_items(component_id: Option<&str>, messages: &[String]) -> Result<AddItemsOutput> {
    // Auto-detect JSON in component_id
    if let Some(input) = component_id {
        if crate::config::is_json_input(input) {
            return add_items_bulk(input);
        }
    }

    let id = component_id.ok_or_else(|| {
        Error::validation_invalid_argument(
            "componentId",
            "Missing componentId (or use --cwd)",
            None,
            None,
        )
    })?;

    if messages.is_empty() {
        return Err(Error::validation_invalid_argument(
            "message",
            "Missing message",
            None,
            None,
        ));
    }

    let component = component::load(id)?;
    let settings = resolve_effective_settings(Some(&component));

    let (path, changed, items_added) =
        read_and_add_next_section_items(&component, &settings, messages)?;

    Ok(AddItemsOutput {
        component_id: id.to_string(),
        changelog_path: path.to_string_lossy().to_string(),
        next_section_label: settings.next_section_label,
        messages: messages.to_vec(),
        items_added,
        changed,
    })
}

// === CWD Changelog Operations ===

/// Default changelog settings for use without a component.
pub fn default_settings() -> EffectiveChangelogSettings {
    resolve_effective_settings(None)
}

/// Well-known changelog file names for auto-detection
const CHANGELOG_CANDIDATES: &[&str] = &[
    "CHANGELOG.md",
    "changelog.md",
    "docs/CHANGELOG.md",
    "docs/changelog.md",
    "HISTORY.md",
];

/// Detect changelog file in a directory by checking for well-known files.
pub fn detect_changelog_path(base_path: &str) -> Option<PathBuf> {
    for candidate in CHANGELOG_CANDIDATES {
        let full_path = Path::new(base_path).join(candidate);
        if full_path.exists() {
            return Some(full_path);
        }
    }
    None
}

/// Add changelog items in the current working directory.
pub fn add_items_cwd(messages: &[String]) -> Result<AddItemsOutput> {
    if messages.is_empty() {
        return Err(Error::validation_invalid_argument(
            "message",
            "Missing message",
            None,
            None,
        ));
    }

    let cwd = std::env::current_dir()
        .map_err(|e| Error::other(format!("Failed to get current directory: {}", e)))?;
    let cwd_str = cwd.to_string_lossy().to_string();

    let changelog_path = detect_changelog_path(&cwd_str).ok_or_else(|| {
        Error::validation_invalid_argument(
            "changelog",
            "No changelog file found in current directory. Looked for: CHANGELOG.md, docs/changelog.md, HISTORY.md, changelog.md",
            None,
            Some(vec![
                "Run `homeboy changelog init --cwd` to create a Keep a Changelog template.".to_string(),
            ]),
        )
    })?;

    let settings = default_settings();
    let content = fs::read_to_string(&changelog_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read changelog".to_string())))?;

    let (updated, changed, items_added) =
        add_next_section_items(&content, &settings.next_section_aliases, messages)?;

    if changed {
        fs::write(&changelog_path, &updated)
            .map_err(|e| Error::internal_io(e.to_string(), Some("write changelog".to_string())))?;
    }

    Ok(AddItemsOutput {
        component_id: "cwd".to_string(),
        changelog_path: changelog_path.to_string_lossy().to_string(),
        next_section_label: settings.next_section_label,
        messages: messages.to_vec(),
        items_added,
        changed,
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
    let component = component::load(component_id)?;
    let settings = resolve_effective_settings(Some(&component));

    // Determine changelog path (relative to component)
    let relative_path = path.unwrap_or("CHANGELOG.md");
    let changelog_path = resolve_target_path(&component.local_path, relative_path)?;

    // Configure component if requested (do this regardless of file state)
    let configured = if configure {
        component::set_changelog_target(component_id, relative_path)?;
        true
    } else {
        false
    };

    // Handle existing file: ensure Unreleased section exists
    if changelog_path.exists() {
        let content = fs::read_to_string(&changelog_path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read changelog".to_string())))?;

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

/// Initialize a changelog in the current working directory.
/// If the changelog file doesn't exist, creates a new one with Keep a Changelog template.
/// If the changelog file exists, ensures it has an Unreleased section.
pub fn init_cwd(path: Option<&str>) -> Result<InitOutput> {
    let cwd = std::env::current_dir()
        .map_err(|e| Error::other(format!("Failed to get current directory: {}", e)))?;
    let settings = default_settings();

    // Determine changelog path
    let changelog_path = if let Some(p) = path {
        cwd.join(p)
    } else {
        cwd.join("CHANGELOG.md")
    };

    // Handle existing file: ensure Unreleased section exists
    if changelog_path.exists() {
        let content = fs::read_to_string(&changelog_path)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read changelog".to_string())))?;

        let (new_content, changed) = ensure_next_section(&content, &settings.next_section_aliases)?;

        if changed {
            local_files::local().write(&changelog_path, &new_content)?;
        }

        return Ok(InitOutput {
            component_id: "cwd".to_string(),
            changelog_path: changelog_path.to_string_lossy().to_string(),
            initial_version: String::new(),
            next_section_label: settings.next_section_label,
            created: false,
            changed,
            configured: false,
        });
    }

    // File doesn't exist: create new changelog with template
    let version_info = version::read_version_cwd()?;
    let initial_version = version_info.version;

    if let Some(parent) = changelog_path.parent() {
        local_files::local().ensure_dir(parent)?;
    }

    let content = generate_template(&initial_version, &settings.next_section_label);
    local_files::local().write(&changelog_path, &content)?;

    Ok(InitOutput {
        component_id: "cwd".to_string(),
        changelog_path: changelog_path.to_string_lossy().to_string(),
        initial_version,
        next_section_label: settings.next_section_label,
        created: true,
        changed: true,
        configured: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_next_section_items_appends_multiple_in_order() {
        let content = "# Changelog\n\n## Unreleased\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string(), "[Unreleased]".to_string()];
        let messages = vec!["First".to_string(), "Second".to_string()];

        let (out, changed, items_added) =
            add_next_section_items(content, &aliases, &messages).unwrap();
        assert!(changed);
        assert_eq!(items_added, 2);
        assert!(out.contains("## Unreleased\n\n- First\n- Second\n\n## 0.1.0"));
    }

    #[test]
    fn add_next_section_items_dedupes_exact_bullets() {
        let content = "# Changelog\n\n## Unreleased\n\n- First\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string(), "[Unreleased]".to_string()];
        let messages = vec!["First".to_string(), "Second".to_string()];

        let (out, changed, items_added) =
            add_next_section_items(content, &aliases, &messages).unwrap();
        assert!(changed);
        assert_eq!(items_added, 1);
        assert!(out.contains("- First"));
        assert!(out.contains("- Second"));
    }

    #[test]
    fn finalize_moves_body_to_new_version_and_resets_next_section() {
        let content = "# Changelog\n\n## Unreleased\n\n- First\n- Second\n\n## 0.1.0\n\n- Old\n";
        let aliases = vec!["Unreleased".to_string(), "[Unreleased]".to_string()];
        let (out, changed) = finalize_next_section(content, &aliases, "0.2.0", false).unwrap();
        assert!(changed);
        let unreleased_pos = out.find("## Unreleased").unwrap();
        let version_pos = out.find("## [0.2.0]").unwrap();
        assert!(
            unreleased_pos < version_pos,
            "## Unreleased should come before ## [0.2.0]"
        );
        // Check for Keep a Changelog format: ## [X.Y.Z] - YYYY-MM-DD
        assert!(out.contains("## [0.2.0] - "));
        assert!(out.contains("- First\n- Second"));
        assert!(out.contains("## 0.1.0"));
    }

    #[test]
    fn finalize_errors_on_empty_next_section_by_default() {
        let content = "# Changelog\n\n## Unreleased\n\n\n## 0.1.0\n\n- Old\n";
        let aliases = vec!["Unreleased".to_string(), "[Unreleased]".to_string()];
        let err = finalize_next_section(content, &aliases, "0.2.0", false).unwrap_err();
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("Invalid"));
    }

    #[test]
    fn finalize_noops_on_empty_when_allowed() {
        let content = "# Changelog\n\n## Unreleased\n\n\n## 0.1.0\n\n- Old\n";
        let aliases = vec!["Unreleased".to_string(), "[Unreleased]".to_string()];
        let (out, changed) = finalize_next_section(content, &aliases, "0.2.0", true).unwrap();
        assert!(!changed);
        assert_eq!(out, content);
    }

    #[test]
    fn get_latest_finalized_version_finds_first_semver() {
        let content = "# Changelog\n\n## Unreleased\n\n## 0.2.16\n\n- Item\n\n## 0.2.15\n";
        assert_eq!(
            get_latest_finalized_version(content),
            Some("0.2.16".to_string())
        );
    }

    #[test]
    fn get_latest_finalized_version_parses_bracketed_format() {
        let content = "# Changelog\n\n## Unreleased\n\n## [1.0.0]\n\n## 0.2.16\n";
        // [1.0.0] is now parsed as 1.0.0 (Keep a Changelog format)
        assert_eq!(
            get_latest_finalized_version(content),
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn get_latest_finalized_version_parses_dated_format() {
        let content = "# Changelog\n\n## Unreleased\n\n## [1.0.0] - 2025-01-14\n\n## 0.2.16\n";
        // Full Keep a Changelog format with date
        assert_eq!(
            get_latest_finalized_version(content),
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn get_latest_finalized_version_returns_none_when_no_versions() {
        let content = "# Changelog\n\n## Unreleased\n\n- Item\n";
        assert_eq!(get_latest_finalized_version(content), None);
    }
}
