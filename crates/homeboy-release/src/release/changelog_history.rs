use std::collections::HashMap;
use std::path::Path;

use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};
use homeboy_core::git;
use homeboy_core::io::{write_output_file_atomically, OutputWriteOptions};

use super::changelog;
use super::scope::ReleaseScope;
use super::types::{
    ChangelogHistoryAffectedVersion, ChangelogHistoryEvidence, ChangelogHistoryRecoveryReport,
};

#[derive(Clone)]
struct Section {
    version: String,
    entries: Vec<(usize, String)>,
}

pub(super) fn recover(
    component: &Component,
    component_id: &str,
    requested_versions: &[String],
    apply: bool,
) -> Result<ChangelogHistoryRecoveryReport> {
    let path = changelog::resolve_changelog_path(component)?;
    let current = std::fs::read_to_string(&path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    let current_sections = sections(&current)?;
    let scope = ReleaseScope::resolve(component, component_id)?;
    let git_root = Path::new(&scope.git_root);
    let relative_path = path.strip_prefix(git_root).map_err(|_| {
        Error::validation_invalid_argument(
            "changelog",
            format!(
                "Changelog {} is outside git root {}",
                path.display(),
                git_root.display()
            ),
            None,
            None,
        )
    })?;
    let relative_path = relative_path.to_string_lossy().replace('\\', "/");
    let mut affected = Vec::new();
    let mut remove_lines = Vec::new();

    let requested_versions = requested_versions
        .iter()
        .map(|version| version.trim().to_string())
        .collect::<Vec<_>>();
    if requested_versions.iter().any(String::is_empty)
        || requested_versions.len()
            != requested_versions
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
    {
        return Err(Error::validation_invalid_argument(
            "repair-changelog-history",
            "Requested finalized versions must be non-empty and unique",
            None,
            None,
        ));
    }

    for version in requested_versions {
        let section = exact_section(&current_sections, &version, "current changelog")?;
        let tag = scope.tag_name(&section.version);
        let tag_commit = git::rev_parse(git_root, &format!("{tag}^{{commit}}"))
            .ok_or_else(|| evidence_error(&section.version, &tag, "tag is missing or ambiguous"))?;
        let baseline = git::output_optional(git_root, &["show", &format!("{tag}:{relative_path}")])
            .ok_or_else(|| {
                evidence_error(
                    &section.version,
                    &tag,
                    "tag does not contain the changelog baseline",
                )
            })?;
        let baseline_sections = sections(&baseline)?;
        let baseline_section =
            exact_section(&baseline_sections, &section.version, "tagged changelog")
                .map_err(|error| evidence_error(&section.version, &tag, &error.message))?;

        let baseline_entries: HashMap<String, usize> = counts(&baseline_section.entries);
        let current_entries: HashMap<String, usize> = counts(&section.entries);
        if baseline_entries
            .iter()
            .any(|(entry, count)| current_entries.get(entry).unwrap_or(&0) < count)
        {
            return Err(evidence_error(
                &section.version,
                &tag,
                "current section is not a superset of its tagged baseline",
            ));
        }

        let mut remaining = baseline_entries;
        let mut extras = Vec::new();
        for (line, entry) in &section.entries {
            match remaining.get_mut(entry) {
                Some(count) if *count > 0 => *count -= 1,
                _ => {
                    extras.push(entry.clone());
                    remove_lines.push(*line);
                }
            }
        }
        if !extras.is_empty() {
            affected.push(ChangelogHistoryAffectedVersion {
                version: section.version.clone(),
                entries: extras,
                evidence: ChangelogHistoryEvidence {
                    tag,
                    tag_commit,
                    changelog_path: relative_path.clone(),
                },
            });
        }
    }

    if apply && !remove_lines.is_empty() {
        let repaired = current
            .lines()
            .enumerate()
            .filter_map(|(index, line)| (!remove_lines.contains(&index)).then_some(line))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        write_output_file_atomically(&path, repaired, OutputWriteOptions::file()).map_err(
            |error| {
                Error::internal_io(error.to_string(), Some(format!("write {}", path.display())))
            },
        )?;
    }

    Ok(ChangelogHistoryRecoveryReport {
        mode: if apply { "apply" } else { "dry_run" }.to_string(),
        applied: apply && !affected.is_empty(),
        affected_versions: affected,
    })
}

fn evidence_error(version: &str, tag: &str, reason: &str) -> Error {
    Error::validation_invalid_argument(
        "repair-changelog-history",
        format!("Refusing changelog history repair for {version}: {reason} ({tag})"),
        None,
        None,
    )
}

fn counts(entries: &[(usize, String)]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for (_, entry) in entries {
        *counts.entry(entry.clone()).or_insert(0) += 1;
    }
    counts
}

fn sections(content: &str) -> Result<Vec<Section>> {
    let lines: Vec<&str> = content.lines().collect();
    let mut headings = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some(version) = line
            .trim()
            .strip_prefix("## [")
            .and_then(|value| value.split_once(']'))
            .map(|(version, _)| version.trim().to_string())
        {
            if version.eq_ignore_ascii_case("unreleased") {
                continue;
            }
            if version.is_empty() {
                return Err(Error::validation_invalid_argument(
                    "repair-changelog-history",
                    "Changelog has an empty or duplicate finalized version heading",
                    None,
                    None,
                ));
            }
            headings.push((index, version));
        }
    }
    Ok(headings
        .iter()
        .enumerate()
        .map(|(position, (start, version))| {
            let end = headings
                .get(position + 1)
                .map(|(start, _)| *start)
                .unwrap_or(lines.len());
            Section {
                version: version.clone(),
                entries: lines[*start + 1..end]
                    .iter()
                    .enumerate()
                    .filter_map(|(offset, line)| {
                        line.trim_start()
                            .strip_prefix("- ")
                            .map(|entry| (*start + 1 + offset, entry.trim().to_string()))
                    })
                    .collect(),
            }
        })
        .collect::<Vec<_>>())
}

fn exact_section<'a>(sections: &'a [Section], version: &str, source: &str) -> Result<&'a Section> {
    let matches = sections
        .iter()
        .filter(|section| section.version == version)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [section] => Ok(*section),
        [] => Err(Error::validation_invalid_argument(
            "repair-changelog-history",
            format!("Requested finalized version {version} is absent from {source}"),
            None,
            None,
        )),
        _ => Err(Error::validation_invalid_argument(
            "repair-changelog-history",
            format!("Requested finalized version {version} is duplicated in {source}"),
            None,
            None,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::recover;
    use homeboy_core::component::Component;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn component(root: &Path) -> Component {
        serde_json::from_value(serde_json::json!({
            "id": "fixture",
            "local_path": root,
            "changelog_target": "CHANGELOG.md"
        }))
        .expect("component")
    }

    fn fixture() -> (tempfile::TempDir, Component) {
        let root = tempfile::tempdir().expect("temporary repository");
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        git(root.path(), &["config", "user.name", "Test"]);
        std::fs::write(
            root.path().join("CHANGELOG.md"),
            "# Changelog\n\n## [1.0.0]\n\n- original entry\n",
        )
        .expect("write changelog");
        git(root.path(), &["add", "CHANGELOG.md"]);
        git(root.path(), &["commit", "-qm", "release: v1.0.0"]);
        git(root.path(), &["tag", "v1.0.0"]);
        std::fs::write(root.path().join("CHANGELOG.md"), "# Changelog\n\n## [1.1.0]\n\n- later release entry\n\n## [1.0.0]\n\n- original entry\n").expect("write second release");
        git(root.path(), &["commit", "-am", "release: v1.1.0", "-q"]);
        git(root.path(), &["tag", "v1.1.0"]);
        let component = component(root.path());
        (root, component)
    }

    #[test]
    fn detects_applies_idempotently_and_preserves_tagged_history() {
        let (root, component) = fixture();
        let path = root.path().join("CHANGELOG.md");
        std::fs::write(&path, "# Changelog\n\n## [Unreleased]\n\n## [1.1.0]\n\n- later release entry\n\n## [1.0.0]\n\n- original entry\n- post-release insertion\n\n## [0.90.1]\n\n- unrelated untagged history\n").expect("insert drift");

        let versions = ["1.0.0".to_string()];
        let detected = recover(&component, "fixture", &versions, false).expect("detect drift");
        assert!(!detected.applied);
        assert_eq!(detected.affected_versions.len(), 1);
        assert_eq!(detected.affected_versions[0].version, "1.0.0");
        assert_eq!(
            detected.affected_versions[0].entries,
            ["post-release insertion"]
        );
        assert!(std::fs::read_to_string(&path)
            .expect("read dry run")
            .contains("post-release insertion"));

        let applied = recover(&component, "fixture", &versions, true).expect("repair drift");
        assert!(applied.applied);
        let repaired = std::fs::read_to_string(&path).expect("read repair");
        assert!(repaired.contains("- original entry"));
        assert!(repaired.contains("- later release entry"));
        assert!(repaired.contains("- unrelated untagged history"));
        assert!(!repaired.contains("post-release insertion"));

        let rerun = recover(&component, "fixture", &versions, true).expect("idempotent repair");
        assert!(!rerun.applied);
        assert!(rerun.affected_versions.is_empty());
    }

    #[test]
    fn refuses_missing_tag_evidence() {
        let (root, component) = fixture();
        let path = root.path().join("CHANGELOG.md");
        std::fs::write(&path, "# Changelog\n\n## [9.9.9]\n\n- unknown release\n\n## [1.1.0]\n\n- later release entry\n\n## [1.0.0]\n\n- original entry\n").expect("write unknown section");

        let versions = ["9.9.9".to_string()];
        let error =
            recover(&component, "fixture", &versions, false).expect_err("missing tag must refuse");
        assert!(error.message.contains("tag is missing or ambiguous"));
        assert!(std::fs::read_to_string(path)
            .expect("read refusal")
            .contains("unknown release"));
    }
}
