//! Documentation generation utilities.
//!
//! Two modes:
//! - **Spec-based**: create markdown files from a JSON spec (`GenerateSpec`).
//! - **From audit**: render detected features into documentation using
//!   extension-configured templates and doc targets.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::code_audit::docs_audit::{AuditResult, DetectedFeature};
use crate::{component, extension, Error};

// ============================================================================
// Types
// ============================================================================

/// Input spec for bulk documentation generation.
#[derive(Deserialize)]
pub struct GenerateSpec {
    pub output_dir: String,
    pub files: Vec<GenerateFileSpec>,
}

/// A single file to generate.
#[derive(Deserialize)]
pub struct GenerateFileSpec {
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

/// Result of a generate operation.
pub struct GenerateResult {
    pub files_created: Vec<String>,
    pub files_updated: Vec<String>,
    pub hints: Vec<String>,
}

// ============================================================================
// Spec-based generation
// ============================================================================

/// Generate documentation files from a [`GenerateSpec`].
///
/// For each file in the spec: creates parent directories, writes content (or
/// derives a title and infers section headings from sibling docs).
pub fn generate_from_spec(spec: &GenerateSpec) -> Result<GenerateResult, Error> {
    let output_path = Path::new(&spec.output_dir);

    if !output_path.exists() {
        fs::create_dir_all(output_path).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("create {}", spec.output_dir)))
        })?;
    }

    let mut files_created = Vec::new();
    let mut files_updated = Vec::new();

    for file_spec in &spec.files {
        let file_path = output_path.join(&file_spec.path);

        if let Some(parent) = file_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
                })?;
            }
        }

        let content = if let Some(ref c) = file_spec.content {
            c.clone()
        } else {
            let title_line = if let Some(ref title) = file_spec.title {
                format!("# {}", title)
            } else {
                let name = file_spec
                    .path
                    .trim_end_matches(".md")
                    .split('/')
                    .next_back()
                    .unwrap_or(&file_spec.path);
                format!("# {}", title_from_name(name))
            };

            let filename = file_spec
                .path
                .split('/')
                .next_back()
                .unwrap_or(&file_spec.path);
            let sibling_dir = if let Some(parent) = file_path.parent() {
                parent.to_path_buf()
            } else {
                output_path.to_path_buf()
            };
            let sections = infer_sections_from_siblings(&sibling_dir, filename);

            if let Some(headings) = sections {
                let mut parts = vec![title_line, String::new()];
                for heading in headings {
                    parts.push(format!("## {}", heading));
                    parts.push(String::new());
                }
                parts.join("\n")
            } else {
                format!("{}\n", title_line)
            }
        };

        let existed = file_path.exists();

        fs::write(&file_path, &content).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("write {}", file_path.display())),
            )
        })?;

        let relative_path = file_path.to_string_lossy().to_string();
        if existed {
            files_updated.push(relative_path);
        } else {
            files_created.push(relative_path);
        }
    }

    let mut hints = Vec::new();
    if !files_created.is_empty() {
        hints.push(format!("Created {} files", files_created.len()));
    }
    if !files_updated.is_empty() {
        hints.push(format!("Updated {} files", files_updated.len()));
    }

    Ok(GenerateResult {
        files_created,
        files_updated,
        hints,
    })
}

// ============================================================================
// From-audit generation
// ============================================================================

/// Generate documentation from audit output (detected features).
///
/// Groups features by extension-configured labels, then renders each group
/// into the appropriate doc file using the configured template and heading.
pub fn generate_from_audit(audit: &AuditResult, dry_run: bool) -> Result<GenerateResult, Error> {
    if audit.detected_features.is_empty() {
        return Err(Error::validation_invalid_argument(
            "from-audit",
            "Audit result has no detected_features. Use `homeboy audit` to generate audit output with features.",
            None,
            Some(vec![
                "homeboy audit <component-id> > audit.json".to_string(),
                "homeboy docs generate --from-audit @audit.json".to_string(),
            ]),
        ));
    }

    let comp = component::load(&audit.component_id).ok();
    let (feature_labels, doc_targets) = collect_extension_doc_config(comp.as_ref());
    let groups = group_features_by_label(&audit.detected_features, &feature_labels);

    let docs_dir = comp
        .as_ref()
        .and_then(|c| c.docs_dir.as_deref())
        .unwrap_or("docs");
    let source_path = comp
        .as_ref()
        .map(|c| Path::new(&c.local_path).to_path_buf())
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let docs_path = source_path.join(docs_dir);

    let mut files_created = Vec::new();
    let mut files_updated = Vec::new();
    let mut hints = Vec::new();

    for (label, features) in &groups {
        let target = match doc_targets.get(label.as_str()) {
            Some(t) => t,
            None => {
                hints.push(format!(
                    "Skipped '{}' ({} features) \u{2014} no doc_target configured in extension",
                    label,
                    features.len()
                ));
                continue;
            }
        };

        let file_path = docs_path.join(&target.file);
        let default_heading = format!("## {}", label);
        let heading = target.heading.as_deref().unwrap_or(&default_heading);
        let template = target
            .template
            .as_deref()
            .unwrap_or("- `{name}` ({source_file}:{line})");

        let mut section_lines: Vec<String> = Vec::new();
        section_lines.push(heading.to_string());
        section_lines.push(String::new());

        for feature in features {
            let desc = feature.description.as_deref().unwrap_or("");
            let has_fields = template.contains("{fields}") && feature.fields.is_some();
            let line = template
                .replace("{name}", &feature.name)
                .replace("{source_file}", &feature.source_file)
                .replace("{line}", &feature.line.to_string())
                .replace("{description}", desc)
                .replace("{fields}", "")
                .replace(
                    "{documented}",
                    if feature.documented {
                        "yes"
                    } else {
                        "**undocumented**"
                    },
                );

            for tpl_line in line.lines() {
                if tpl_line.trim().is_empty() {
                    continue;
                }
                section_lines.push(tpl_line.to_string());
            }

            if has_fields {
                section_lines.push(String::new());
                for field in feature.fields.as_ref().unwrap() {
                    let field_desc = field.description.as_deref().unwrap_or("");
                    if field_desc.is_empty() {
                        section_lines.push(format!("- `{}`", field.name));
                    } else {
                        section_lines.push(format!("- `{}` \u{2014} {}", field.name, field_desc));
                    }
                }
            }

            section_lines.push(String::new());
        }
        section_lines.push(String::new());

        let section_content = section_lines.join("\n");

        let existed = file_path.exists();
        let final_content = if existed {
            let existing = fs::read_to_string(&file_path).unwrap_or_default();
            replace_or_append_section(&existing, heading, &section_content)
        } else {
            let title = format!("# {}\n\n", label);
            format!("{}{}", title, section_content)
        };

        if !dry_run {
            if let Some(parent) = file_path.parent() {
                if !parent.exists() {
                    fs::create_dir_all(parent).map_err(|e| {
                        Error::internal_io(
                            e.to_string(),
                            Some(format!("create {}", parent.display())),
                        )
                    })?;
                }
            }
            fs::write(&file_path, &final_content).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("write {}", file_path.display())),
                )
            })?;
        }

        let relative = format!("{}/{}", docs_dir, target.file);
        if existed {
            files_updated.push(relative);
        } else {
            files_created.push(relative);
        }
    }

    if dry_run {
        hints.insert(0, "Dry run \u{2014} no files written".to_string());
    }

    // Deduplicate
    let mut seen = std::collections::HashSet::new();
    files_created.retain(|f| seen.insert(f.clone()));
    seen.clear();
    files_updated.retain(|f| seen.insert(f.clone()));
    files_updated.retain(|f| !files_created.contains(f));

    Ok(GenerateResult {
        files_created,
        files_updated,
        hints,
    })
}

// ============================================================================
// Helpers
// ============================================================================

/// Collect feature_labels and doc_targets from all linked extensions.
fn collect_extension_doc_config(
    comp: Option<&component::Component>,
) -> (
    HashMap<String, String>,
    HashMap<String, extension::DocTarget>,
) {
    let mut labels = HashMap::new();
    let mut targets = HashMap::new();

    if let Some(comp) = comp {
        if let Some(ref extensions) = comp.extensions {
            for extension_id in extensions.keys() {
                if let Ok(manifest) = extension::load_extension(extension_id) {
                    for (key, label) in manifest.audit_feature_labels() {
                        labels.insert(key.clone(), label.clone());
                    }
                    for (label, target) in manifest.audit_doc_targets() {
                        targets.insert(label.clone(), target.clone());
                    }
                }
            }
        }
    }

    (labels, targets)
}

/// Group detected features by their label (resolved from pattern → label mapping).
fn group_features_by_label<'a>(
    features: &'a [DetectedFeature],
    feature_labels: &HashMap<String, String>,
) -> Vec<(String, Vec<&'a DetectedFeature>)> {
    let mut groups: HashMap<String, Vec<&'a DetectedFeature>> = HashMap::new();

    for feature in features {
        let label = feature_labels
            .iter()
            .find(|(key, _)| feature.pattern.contains(key.as_str()))
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| feature.pattern.clone());

        groups.entry(label).or_default().push(feature);
    }

    let mut sorted: Vec<(String, Vec<&DetectedFeature>)> = groups.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    sorted
}

/// Replace an existing section in a doc file, or append it.
///
/// A "section" starts with the heading line and ends at the next heading of equal
/// or higher level, or end of file.
fn replace_or_append_section(existing: &str, heading: &str, new_section: &str) -> String {
    let heading_level = heading.chars().take_while(|c| *c == '#').count();
    let lines: Vec<&str> = existing.lines().collect();

    let start = lines.iter().position(|line| line.trim() == heading);

    if let Some(start_idx) = start {
        let end_idx = lines[start_idx + 1..]
            .iter()
            .position(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with('#') {
                    let level = trimmed.chars().take_while(|c| *c == '#').count();
                    level <= heading_level
                } else {
                    false
                }
            })
            .map(|i| start_idx + 1 + i)
            .unwrap_or(lines.len());

        let mut result: Vec<&str> = Vec::new();
        result.extend_from_slice(&lines[..start_idx]);
        let new_lines: Vec<&str> = new_section.lines().collect();
        result.extend(new_lines);
        if end_idx < lines.len() {
            result.extend_from_slice(&lines[end_idx..]);
        }
        result.join("\n")
    } else {
        let mut result = existing.to_string();
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result.push('\n');
        result.push_str(new_section);
        result
    }
}

/// Infer common section headings from sibling markdown files in the same directory.
///
/// Reads all `.md` files in `dir` (excluding `exclude_filename`), extracts `## ` headings,
/// and returns the ordered list of headings that appear in at least 3 files or 50% of
/// siblings (whichever threshold is lower).
///
/// Returns `None` if fewer than 3 siblings exist or no common headings are found.
fn infer_sections_from_siblings(dir: &Path, exclude_filename: &str) -> Option<Vec<String>> {
    if !dir.is_dir() {
        return None;
    }

    let entries = fs::read_dir(dir).ok()?;
    let mut sibling_headings: Vec<Vec<String>> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if !name.ends_with(".md") || name == exclude_filename || !path.is_file() {
            continue;
        }

        let content = fs::read_to_string(&path).ok();
        if let Some(text) = content {
            let headings: Vec<String> = text
                .lines()
                .filter_map(|line| {
                    let trimmed = line.trim();
                    if trimmed.starts_with("## ") && !trimmed.starts_with("### ") {
                        Some(trimmed.trim_start_matches("## ").trim().to_string())
                    } else {
                        None
                    }
                })
                .collect();

            if !headings.is_empty() {
                sibling_headings.push(headings);
            }
        }
    }

    let sibling_count = sibling_headings.len();
    if sibling_count < 3 {
        return None;
    }

    let mut heading_counts: HashMap<String, usize> = HashMap::new();
    for headings in &sibling_headings {
        let unique: std::collections::HashSet<&String> = headings.iter().collect();
        for heading in unique {
            *heading_counts.entry(heading.clone()).or_insert(0) += 1;
        }
    }

    let threshold = std::cmp::min(3, sibling_count.div_ceil(2));

    let common_set: std::collections::HashSet<&str> = heading_counts
        .iter()
        .filter(|(_, count)| **count >= threshold)
        .map(|(name, _)| name.as_str())
        .collect();

    if common_set.is_empty() {
        return None;
    }

    let mut median_positions: HashMap<&str, usize> = HashMap::new();
    for heading in &common_set {
        let mut positions: Vec<usize> = Vec::new();
        for headings in &sibling_headings {
            if let Some(pos) = headings.iter().position(|h| h == heading) {
                positions.push(pos);
            }
        }
        positions.sort();
        let median = positions[positions.len() / 2];
        median_positions.insert(heading, median);
    }

    let mut common_headings: Vec<String> = common_set.iter().map(|s| s.to_string()).collect();
    common_headings.sort_by_key(|h| {
        median_positions
            .get(h.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });

    if common_headings.is_empty() {
        None
    } else {
        Some(common_headings)
    }
}

/// Convert kebab-case or snake_case to Title Case.
fn title_from_name(name: &str) -> String {
    name.split(['-', '_'])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("Failed to create temp dir")
    }

    fn write_md(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).expect("Failed to write test file");
    }

    #[test]
    fn test_infer_sections_returns_none_when_fewer_than_3_siblings() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Config\n\n## Usage\n");
        write_md(dir, "b.md", "# B\n\n## Config\n\n## Usage\n");

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_none(), "Should return None with only 2 siblings");
    }

    #[test]
    fn test_infer_sections_finds_common_headings() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(
            dir,
            "a.md",
            "# A\n\n## Configuration\n\n## Parameters\n\n## Error Handling\n",
        );
        write_md(
            dir,
            "b.md",
            "# B\n\n## Configuration\n\n## Parameters\n\n## Error Handling\n",
        );
        write_md(
            dir,
            "c.md",
            "# C\n\n## Configuration\n\n## Parameters\n\n## Error Handling\n",
        );

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_some(), "Should find common headings");
        let headings = result.unwrap();
        assert_eq!(
            headings,
            vec!["Configuration", "Parameters", "Error Handling"]
        );
    }

    #[test]
    fn test_infer_sections_excludes_target_file() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Config\n\n## Usage\n");
        write_md(dir, "b.md", "# B\n\n## Config\n\n## Usage\n");
        write_md(dir, "c.md", "# C\n\n## Config\n\n## Usage\n");
        write_md(dir, "new.md", "# New\n\n## Totally Different\n");

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_some());
        let headings = result.unwrap();
        assert!(headings.contains(&"Config".to_string()));
        assert!(headings.contains(&"Usage".to_string()));
        assert!(!headings.contains(&"Totally Different".to_string()));
    }

    #[test]
    fn test_infer_sections_filters_uncommon_headings() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(
            dir,
            "a.md",
            "# A\n\n## Config\n\n## Usage\n\n## Special A\n",
        );
        write_md(
            dir,
            "b.md",
            "# B\n\n## Config\n\n## Usage\n\n## Special B\n",
        );
        write_md(dir, "c.md", "# C\n\n## Config\n\n## Usage\n");

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_some());
        let headings = result.unwrap();
        assert_eq!(headings, vec!["Config", "Usage"]);
    }

    #[test]
    fn test_infer_sections_returns_none_for_nonexistent_dir() {
        let result = infer_sections_from_siblings(Path::new("/nonexistent/path"), "new.md");
        assert!(result.is_none());
    }

    #[test]
    fn test_infer_sections_skips_non_md_files() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Config\n");
        write_md(dir, "b.md", "# B\n\n## Config\n");
        write_md(dir, "c.md", "# C\n\n## Config\n");
        fs::write(dir.join("readme.txt"), "## Not Markdown\n").unwrap();

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_some());
    }

    #[test]
    fn test_infer_sections_ignores_h3_headings() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Config\n\n### Sub Detail\n");
        write_md(dir, "b.md", "# B\n\n## Config\n\n### Sub Detail\n");
        write_md(dir, "c.md", "# C\n\n## Config\n\n### Sub Detail\n");

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_some());
        let headings = result.unwrap();
        assert_eq!(headings, vec!["Config"]);
        assert!(!headings.contains(&"Sub Detail".to_string()));
    }

    #[test]
    fn test_infer_sections_returns_none_when_no_common_pattern() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Alpha\n");
        write_md(dir, "b.md", "# B\n\n## Beta\n");
        write_md(dir, "c.md", "# C\n\n## Gamma\n");

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_none(), "No heading appears in 3+ files");
    }

    #[test]
    fn test_title_from_name_kebab_case() {
        assert_eq!(title_from_name("google-analytics"), "Google Analytics");
    }

    #[test]
    fn test_title_from_name_snake_case() {
        assert_eq!(title_from_name("page_speed"), "Page Speed");
    }
}
