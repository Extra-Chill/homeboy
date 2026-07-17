use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalLabRunEvidence {
    pub(super) run_id: String,
    pub(super) run_dir: PathBuf,
    pub(super) summary_path: PathBuf,
    pub(super) manifest_path: PathBuf,
    pub(super) passed_count: u64,
    pub(super) status: Option<String>,
}

pub(super) fn terminal_lab_run_evidence(
    args: &[String],
    source_path: &Path,
) -> Option<TerminalLabRunEvidence> {
    let run_id = lab_command_run_id(args)?;
    let mut candidates = lab_run_output_candidates(args, source_path, &run_id);
    candidates.sort();
    candidates.dedup();
    candidates
        .into_iter()
        .find_map(|candidate| terminal_lab_run_evidence_at(&run_id, candidate))
}

pub(super) fn lab_command_run_id(args: &[String]) -> Option<String> {
    let mut run_id = None;
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--run-id" {
            run_id = iter.next().cloned();
            continue;
        }
        if let Some(value) = arg.strip_prefix("--run-id=") {
            if !value.is_empty() {
                run_id = Some(value.to_string());
            }
        }
    }
    run_id
}

fn lab_run_output_candidates(args: &[String], source_path: &Path, run_id: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for explicit in lab_explicit_output_paths(args, source_path) {
        candidates.extend(run_dir_candidates(explicit, run_id));
    }
    for status_parent in lab_status_file_parent_paths(args, source_path) {
        candidates.extend(run_dir_candidates(status_parent, run_id));
    }
    candidates.extend([
        source_path.join(run_id),
        source_path.join("runs").join(run_id),
        source_path.join("artifacts").join(run_id),
        source_path.join(".homeboy").join("runs").join(run_id),
        source_path.join(".homeboy").join("artifacts").join(run_id),
        source_path.join("bench-runs").join(run_id),
    ]);
    candidates.extend(lab_run_summary_dirs_from_tree(source_path, run_id));
    candidates
}

fn run_dir_candidates(path: PathBuf, run_id: &str) -> Vec<PathBuf> {
    vec![
        path.clone(),
        path.join(run_id),
        path.join("runs").join(run_id),
    ]
}

fn lab_explicit_output_paths(args: &[String], source_path: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--output-dir" | "--output-root" | "--artifact-dir" | "--artifacts-dir" => {
                if let Some(path) = iter.next() {
                    paths.push(resolve_lab_arg_path(path, source_path));
                }
            }
            _ => {
                for prefix in [
                    "--output-dir=",
                    "--output-root=",
                    "--artifact-dir=",
                    "--artifacts-dir=",
                ] {
                    if let Some(path) = arg.strip_prefix(prefix) {
                        if !path.is_empty() {
                            paths.push(resolve_lab_arg_path(path, source_path));
                        }
                    }
                }
            }
        }
    }
    paths
}

fn lab_status_file_parent_paths(args: &[String], source_path: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let path = if arg == "--status-file" {
            iter.next()
                .map(|value| resolve_lab_arg_path(value, source_path))
        } else {
            arg.strip_prefix("--status-file=")
                .filter(|value| !value.is_empty())
                .map(|value| resolve_lab_arg_path(value, source_path))
        };
        if let Some(path) = path.and_then(|path| path.parent().map(Path::to_path_buf)) {
            paths.push(path);
        }
    }
    paths
}

fn resolve_lab_arg_path(path: &str, source_path: &Path) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        source_path.join(path)
    }
}

fn lab_run_summary_dirs_from_tree(source_path: &Path, run_id: &str) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 6;
    const MAX_DIRS: usize = 2048;

    let mut dirs = Vec::new();
    let mut stack = vec![(source_path.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        visited += 1;
        if visited > MAX_DIRS {
            break;
        }
        let summary = dir.join("homeboy-summary.json");
        if summary.is_file() && path_contains_component(&dir, run_id) {
            dirs.push(dir.clone());
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && !is_hidden_or_heavy_lab_scan_dir(&path) {
                stack.push((path, depth + 1));
            }
        }
    }
    dirs
}

fn path_contains_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str().to_string_lossy() == expected)
}

fn is_hidden_or_heavy_lab_scan_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git" | "node_modules" | "vendor" | "target" | ".next" | "dist" | "build"
    )
}

fn terminal_lab_run_evidence_at(run_id: &str, run_dir: PathBuf) -> Option<TerminalLabRunEvidence> {
    let summary_path = run_dir.join("homeboy-summary.json");
    let manifest_path = run_dir.join("published").join("manifest.json");
    if !summary_path.is_file() || !manifest_path.is_file() {
        return None;
    }
    let summary: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&summary_path).ok()?).ok()?;
    if !summary_matches_lab_run(&summary, run_id) && !path_contains_component(&run_dir, run_id) {
        return None;
    }
    let passed_count = summary
        .get("result_counts")
        .and_then(|counts| counts.get("passed"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let status = summary
        .get("status")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    if passed_count == 0
        && !matches!(
            status.as_deref(),
            Some("passed" | "completed" | "success" | "succeeded")
        )
    {
        return None;
    }
    Some(TerminalLabRunEvidence {
        run_id: run_id.to_string(),
        run_dir,
        summary_path,
        manifest_path,
        passed_count,
        status,
    })
}

fn summary_matches_lab_run(summary: &serde_json::Value, run_id: &str) -> bool {
    for key in ["run_id", "id"] {
        if let Some(value) = summary.get(key).and_then(serde_json::Value::as_str) {
            return value == run_id;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lab_run_idempotency_guard_detects_passed_published_run() {
        let source = tempfile::tempdir().expect("source tempdir");
        let run_dir = source.path().join("runs/studio-web-r10");
        std::fs::create_dir_all(run_dir.join("published")).expect("mkdir published");
        std::fs::write(
            run_dir.join("homeboy-summary.json"),
            serde_json::json!({
                "run_id": "studio-web-r10",
                "status": "completed",
                "result_counts": { "passed": 1 }
            })
            .to_string(),
        )
        .expect("write summary");
        std::fs::write(run_dir.join("published/manifest.json"), "{}").expect("write manifest");

        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "studio-web".to_string(),
            "--run-id".to_string(),
            "studio-web-r10".to_string(),
        ];

        let published = terminal_lab_run_evidence(&args, source.path())
            .expect("published passing run should be terminal");

        assert_eq!(published.run_id, "studio-web-r10");
        assert_eq!(published.passed_count, 1);
        assert_eq!(
            published.manifest_path,
            run_dir.join("published/manifest.json")
        );
    }

    #[test]
    fn lab_run_idempotency_guard_ignores_unpublished_or_failed_runs() {
        let source = tempfile::tempdir().expect("source tempdir");
        let run_dir = source.path().join("runs/studio-web-r11");
        std::fs::create_dir_all(&run_dir).expect("mkdir run");
        std::fs::write(
            run_dir.join("homeboy-summary.json"),
            serde_json::json!({
                "run_id": "studio-web-r11",
                "status": "failed",
                "result_counts": { "passed": 0 }
            })
            .to_string(),
        )
        .expect("write summary");
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "studio-web".to_string(),
            "--run-id=studio-web-r11".to_string(),
        ];

        assert!(terminal_lab_run_evidence(&args, source.path()).is_none());

        std::fs::create_dir_all(run_dir.join("published")).expect("mkdir published");
        std::fs::write(run_dir.join("published/manifest.json"), "{}").expect("write manifest");
        assert!(terminal_lab_run_evidence(&args, source.path()).is_none());
    }

    #[test]
    fn lab_run_idempotency_guard_resolves_relative_explicit_output_dir() {
        let source = tempfile::tempdir().expect("source tempdir");
        let run_dir = source.path().join("relative-output/runs/site-run-1");
        std::fs::create_dir_all(run_dir.join("published")).expect("mkdir published");
        std::fs::write(
            run_dir.join("homeboy-summary.json"),
            serde_json::json!({
                "status": "completed",
                "result_counts": { "passed": 1 }
            })
            .to_string(),
        )
        .expect("write summary");
        std::fs::write(run_dir.join("published/manifest.json"), "{}").expect("write manifest");
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "site".to_string(),
            "--run-id=site-run-1".to_string(),
            "--output-dir".to_string(),
            "relative-output".to_string(),
        ];

        let terminal = terminal_lab_run_evidence(&args, source.path())
            .expect("relative output dir should resolve against source path");

        assert_eq!(terminal.run_dir, run_dir);
        assert_eq!(terminal.passed_count, 1);
    }

    #[test]
    fn lab_run_idempotency_guard_rejects_summary_without_matching_run_scope() {
        let source = tempfile::tempdir().expect("source tempdir");
        let run_dir = source.path().join("relative-output/current");
        std::fs::create_dir_all(run_dir.join("published")).expect("mkdir published");
        std::fs::write(
            run_dir.join("homeboy-summary.json"),
            serde_json::json!({
                "status": "completed",
                "result_counts": { "passed": 1 }
            })
            .to_string(),
        )
        .expect("write summary");
        std::fs::write(run_dir.join("published/manifest.json"), "{}").expect("write manifest");
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "site".to_string(),
            "--run-id=site-run-2".to_string(),
            "--output-dir=relative-output/current".to_string(),
        ];

        assert!(terminal_lab_run_evidence(&args, source.path()).is_none());
    }
}
