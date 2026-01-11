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
        0 => Err(Error::Config(format!(
            "No changelog file found for component at '{}'. Create CHANGELOG.md (preferred) or docs/changelog.md, or set component.changelogTargets[0].file",
            local_path
        ))),
        _ => Err(Error::Config(format!(
            "Multiple changelog files found for component at '{}': {}. Set component.changelogTargets[0].file to disambiguate.",
            local_path,
            hits.iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

pub fn add_next_section_item(
    changelog_content: &str,
    next_section_aliases: &[String],
    message: &str,
) -> Result<(String, bool)> {
    let trimmed_message = message.trim();
    if trimmed_message.is_empty() {
        return Err(Error::Other(
            "Changelog message cannot be empty".to_string(),
        ));
    }

    let (with_section, section_changed) =
        ensure_next_section(changelog_content, next_section_aliases)?;
    let (with_item, item_changed) =
        append_item_to_next_section(&with_section, next_section_aliases, trimmed_message)?;

    Ok((with_item, section_changed || item_changed))
}

pub fn read_and_add_next_section_item(
    component: &ComponentConfiguration,
    settings: &EffectiveChangelogSettings,
    message: &str,
) -> Result<(PathBuf, bool)> {
    let path = resolve_changelog_path(component)?;
    let content = fs::read_to_string(&path)?;

    let (new_content, changed) =
        add_next_section_item(&content, &settings.next_section_aliases, message)?;

    if changed {
        fs::write(&path, new_content)?;
    }

    Ok((path, changed))
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
    let start = find_next_section_start(&lines, aliases)
        .ok_or_else(|| Error::Other("Next changelog section not found (unexpected)".to_string()))?;

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
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(&bullet);
            out.push('\n');
        }
    }

    Ok((out, true))
}
