//! Repository-wide rename discovery for changed-since lint baselines.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy_core::finding::HomeboyFinding;

type RenamePaths = Vec<(String, String)>;
type SourceGroups = BTreeMap<PathBuf, RenamePaths>;

/// Group incoming renames by their nearest portable component at the base ref.
/// Diff the whole repository: path-limiting to the destination hides the source.
pub(crate) fn source_groups(
    repo: &Path,
    checkout: &Path,
    suffix: &Path,
    base: &str,
    files: &[String],
) -> homeboy_core::Result<SourceGroups> {
    let output = Command::new("git")
        .current_dir(repo)
        .args([
            "diff",
            "-M",
            "--name-status",
            "-z",
            "--diff-filter=R",
            base,
            "--",
        ])
        .output()
        .map_err(|error| homeboy_core::Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(homeboy_core::Error::git_command_failed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    let fields: Vec<_> = output.stdout.split(|byte| *byte == 0).collect();
    let mut groups = BTreeMap::new();
    for record in fields.chunks_exact(3) {
        let old = PathBuf::from(String::from_utf8_lossy(record[1]).into_owned());
        let new = PathBuf::from(String::from_utf8_lossy(record[2]).into_owned());
        let Ok(destination) = new.strip_prefix(suffix) else {
            continue;
        };
        let destination = destination.to_string_lossy().into_owned();
        if !files.contains(&destination) {
            continue;
        }
        let Some(owner) = old.parent().and_then(|parent| {
            parent
                .ancestors()
                .find(|ancestor| checkout.join(ancestor).join("homeboy.json").is_file())
        }) else {
            continue;
        };
        let source = old
            .strip_prefix(owner)
            .expect("ancestor owns file")
            .to_string_lossy()
            .into_owned();
        groups
            .entry(owner.to_path_buf())
            .or_insert_with(Vec::new)
            .push((source, destination));
    }
    Ok(groups)
}

/// Only findings attached to a renamed file may cross a component boundary.
pub(crate) fn rewrite(
    findings: Vec<HomeboyFinding>,
    paths: &[(String, String)],
) -> Vec<HomeboyFinding> {
    findings
        .into_iter()
        .filter_map(|mut finding| {
            let file = finding.location.file.as_deref()?;
            let (old, new) = paths.iter().find(|(old, _)| old == file)?;
            if let Some(fingerprint) = &mut finding.fingerprint {
                *fingerprint = fingerprint.replace(old, new);
            }
            finding.location.file = Some(new.clone());
            Some(finding)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_matching_files_are_transferred_and_fingerprints_follow_paths() {
        let findings = vec![
            HomeboyFinding::builder("lint", "existing")
                .file("src/old.php")
                .fingerprint("src/old.php:7:rule")
                .build(),
            HomeboyFinding::builder("lint", "unrelated")
                .file("other.php")
                .fingerprint("other.php:7:rule")
                .build(),
            HomeboyFinding::builder("lint", "global")
                .fingerprint("global")
                .build(),
        ];
        let rewritten = rewrite(findings, &[("src/old.php".into(), "new.php".into())]);
        assert_eq!(rewritten.len(), 1);
        assert_eq!(rewritten[0].location.file.as_deref(), Some("new.php"));
        assert_eq!(rewritten[0].fingerprint.as_deref(), Some("new.php:7:rule"));
    }

    #[test]
    fn repository_diff_detects_cross_component_moves_with_spaces() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .current_dir(repo)
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::create_dir(repo.join("source")).unwrap();
        std::fs::create_dir(repo.join("destination")).unwrap();
        std::fs::write(repo.join("source/homeboy.json"), "{}").unwrap();
        std::fs::write(repo.join("source/old name.php"), "unchanged content\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["mv", "source/old name.php", "destination/new name.php"]);
        let groups = source_groups(
            repo,
            repo,
            Path::new("destination"),
            "HEAD",
            &["new name.php".into()],
        )
        .unwrap();
        assert_eq!(
            groups[Path::new("source")],
            vec![("old name.php".into(), "new name.php".into())]
        );
        assert!(source_groups(
            repo,
            repo,
            Path::new("destination"),
            "HEAD",
            &["unrelated.php".into()]
        )
        .unwrap()
        .is_empty());
    }
}
