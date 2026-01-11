use crate::config::{ComponentConfiguration, ConfigManager, ProjectConfiguration};
use crate::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct EffectiveChangelogSettings {
    pub next_section_label: String,
    pub next_section_aliases: Vec<String>,
}

pub fn resolve_effective_settings(
    project: Option<&ProjectConfiguration>,
    component: Option<&ComponentConfiguration>,
) -> Result<EffectiveChangelogSettings> {
    let app = ConfigManager::load_app_config()?;

    let next_section_label = component
        .and_then(|c| c.changelog_next_section_label.clone())
        .or_else(|| project.and_then(|p| p.changelog_next_section_label.clone()))
        .or_else(|| app.default_changelog_next_section_label.clone())
        .unwrap_or_else(|| "Unreleased".to_string());

    let mut next_section_aliases = component
        .and_then(|c| c.changelog_next_section_aliases.clone())
        .or_else(|| project.and_then(|p| p.changelog_next_section_aliases.clone()))
        .or_else(|| app.default_changelog_next_section_aliases.clone())
        .unwrap_or_default();

    if next_section_aliases.is_empty() {
        next_section_aliases.extend([
            next_section_label.clone(),
            format!("[{}]", next_section_label),
        ]);
    }

    Ok(EffectiveChangelogSettings {
        next_section_label,
        next_section_aliases,
    })
}

pub fn resolve_changelog_path(component: &ComponentConfiguration) -> Result<PathBuf> {
    if let Some(target) = component
        .changelog_targets
        .as_ref()
        .and_then(|targets| targets.first())
    {
        return resolve_target_path(&component.local_path, &target.file);
    }

    autodetect_changelog_path(&component.local_path)
}

fn resolve_target_path(local_path: &str, file: &str) -> Result<PathBuf> {
    let path = if file.starts_with('/') {
        PathBuf::from(file)
    } else {
        Path::new(local_path).join(file)
    };

    Ok(path)
}

fn autodetect_changelog_path(local_path: &str) -> Result<PathBuf> {
    let candidates = ["CHANGELOG.md", "docs/changelog.md"];
    let mut hits = Vec::new();

    for rel in candidates {
        let path = Path::new(local_path).join(rel);
        if path.exists() {
            hits.push(path);
        }
    }

    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => Err(Error::validation_invalid_argument(
            "component.changelogTargets",
            format!(
                "No changelog file found for component at '{}'. Create CHANGELOG.md (preferred) or docs/changelog.md, or set component.changelogTargets[0].file",
                local_path
            ),
            None,
            None,
        )),
        _ => Err(Error::validation_invalid_argument(
            "component.changelogTargets[0].file",
            format!(
                "Multiple changelog files found for component at '{}': {}. Set component.changelogTargets[0].file to disambiguate.",
                local_path,
                hits.iter()
                    .map(|p| p.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None,
            None,
        )),
    }
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
    component: &ComponentConfiguration,
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
    component: &ComponentConfiguration,
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
            None,
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
            None,
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

    // Replace old ## Unreleased with ## <new_version>.
    out_lines.push(format!("## {}", new_version.trim()));

    // Copy everything after the old heading (body + rest of file).
    for line in &lines[start + 1..] {
        out_lines.push((*line).to_string());
    }

    let mut out = out_lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }

    Ok((out, true))
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
        assert!(out.contains("## Unreleased\n\n- First\n\n- Second"));
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
        // ## Unreleased should be at the top (before ## 0.2.0)
        let unreleased_pos = out.find("## Unreleased").unwrap();
        let version_pos = out.find("## 0.2.0").unwrap();
        assert!(
            unreleased_pos < version_pos,
            "## Unreleased should come before ## 0.2.0"
        );
        assert!(out.contains("## 0.2.0\n\n- First\n- Second"));
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
        if trimmed.starts_with("## ") || trimmed == "##" {
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
        out.push_str("\n");
    }

    Ok((out, true))
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

    let end = find_section_end(&lines, start);
    let bullet = format!("- {}", message);

    // If exact bullet already exists inside the section, no-op.
    for line in &lines[start + 1..end] {
        if line.trim() == bullet {
            return Ok((content.to_string(), false));
        }
    }

    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        out.push_str(line);
        out.push('\n');

        if idx + 1 == end {
            out.push_str(&bullet);
            out.push_str("\n\n");
        }
    }

    Ok((out, true))
}
