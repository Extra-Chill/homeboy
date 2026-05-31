//! Trace overlay application and cleanup.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::engine::detail_output::{bounded_items, DEFAULT_DETAIL_ITEM_LIMIT};
use crate::core::engine::run_dir::RunDir;
use crate::core::error::{Error, Result};

use super::overlay_lock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceOverlayRequest {
    pub variant: Option<String>,
    pub component_id: Option<String>,
    pub component_path: String,
    pub overlay_path: String,
}

#[derive(Debug, Clone)]
pub(super) struct AppliedTraceOverlay {
    pub variant: Option<String>,
    pub component_id: Option<String>,
    pub component_path: PathBuf,
    pub patch_path: PathBuf,
    pub touched_files: Vec<String>,
    pub keep: bool,
}

pub(super) fn acquire_trace_overlay_locks(
    requests: &[TraceOverlayRequest],
    run_dir: &RunDir,
) -> Result<Vec<overlay_lock::TraceOverlayLock>> {
    let mut component_requests: BTreeMap<String, (PathBuf, Vec<String>)> = BTreeMap::new();
    for request in requests {
        let normalized = overlay_lock::normalize_component_path(Path::new(&request.component_path));
        let (_, overlay_paths) = component_requests
            .entry(normalized.to_string_lossy().to_string())
            .or_insert_with(|| (normalized, Vec::new()));
        overlay_paths.push(request.overlay_path.clone());
    }

    let mut locks = Vec::new();
    for (component_path, overlay_paths) in component_requests.values() {
        locks.push(overlay_lock::TraceOverlayLock::acquire(
            component_path,
            overlay_paths,
            run_dir,
        )?);
    }
    Ok(locks)
}

pub(super) fn apply_trace_overlays(
    requests: &[TraceOverlayRequest],
    keep: bool,
) -> Result<Vec<AppliedTraceOverlay>> {
    let mut applied = Vec::new();
    for request in requests {
        let component_path = PathBuf::from(&request.component_path);
        let patch_path = PathBuf::from(&request.overlay_path);
        let touched_files = match overlay_touched_files(&component_path, &patch_path) {
            Ok(files) => files,
            Err(error) => return cleanup_with_overlay_error(&applied, keep, error, request),
        };
        if let Err(error) =
            ensure_overlay_targets_clean(&component_path, &patch_path, &touched_files)
        {
            return cleanup_with_overlay_error(&applied, keep, error, request);
        }
        if let Err(error) = run_git_apply(&component_path, &patch_path, false) {
            return cleanup_with_overlay_error(&applied, keep, error, request);
        }
        print_trace_overlay("applied", &patch_path, &touched_files, keep);
        applied.push(AppliedTraceOverlay {
            variant: request.variant.clone(),
            component_id: request.component_id.clone(),
            component_path: component_path.clone(),
            patch_path,
            touched_files,
            keep,
        });
    }
    Ok(applied)
}

fn cleanup_with_overlay_error<T>(
    applied: &[AppliedTraceOverlay],
    keep: bool,
    error: Error,
    request: &TraceOverlayRequest,
) -> Result<T> {
    cleanup_after_overlay_error(applied, keep, trace_overlay_request_error(error, request))
}

fn trace_overlay_request_error(mut error: Error, request: &TraceOverlayRequest) -> Error {
    let component = request
        .component_id
        .as_deref()
        .unwrap_or("<unknown-component>");
    error.message = format!(
        "Trace overlay failed for component '{}' at {} using {}: {}",
        component, request.component_path, request.overlay_path, error.message
    );
    if let Some(details) = error.details.as_object_mut() {
        details.insert(
            "component_id".to_string(),
            serde_json::json!(request.component_id.clone()),
        );
        details.insert(
            "component_path".to_string(),
            serde_json::json!(request.component_path.clone()),
        );
        details.insert(
            "overlay_path".to_string(),
            serde_json::json!(request.overlay_path.clone()),
        );
    }
    error
}

pub(super) fn cleanup_after_overlay_error<T>(
    applied: &[AppliedTraceOverlay],
    keep: bool,
    error: Error,
) -> Result<T> {
    if !keep {
        let _ = cleanup_trace_overlays(applied);
    }
    Err(error)
}

pub(super) fn cleanup_trace_overlays(applied: &[AppliedTraceOverlay]) -> Result<()> {
    let mut first_error = None;
    for overlay in applied.iter().rev() {
        match run_git_apply(&overlay.component_path, &overlay.patch_path, true) {
            Ok(()) => print_trace_overlay(
                "reverted",
                &overlay.patch_path,
                &overlay.touched_files,
                overlay.keep,
            ),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn print_trace_overlay(action: &str, patch_path: &Path, touched_files: &[String], keep: bool) {
    eprintln!("trace overlay {action}: {}", patch_path.display());
    let retention = if action == "reverted" {
        "overlay changes reverted"
    } else if keep {
        "overlay changes will be kept"
    } else {
        "overlay changes will be reverted after the run"
    };
    eprintln!("  status: {retention}");
    if touched_files.is_empty() {
        eprintln!("  touched files: none reported by git apply --numstat");
        return;
    }
    eprintln!("  touched files:");
    let (visible_files, metadata) = bounded_items(touched_files, DEFAULT_DETAIL_ITEM_LIMIT);
    for file in visible_files {
        eprintln!("    - {file}");
    }
    if metadata.truncated {
        eprintln!(
            "    ... {} touched file(s) omitted (shown: {}, total: {}, limit: {})",
            metadata.omitted_item_count,
            metadata.items_rendered,
            metadata.items_seen,
            metadata.item_limit
        );
    }
}

pub(super) fn overlay_touched_files(
    component_path: &Path,
    patch_path: &Path,
) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["apply", "--numstat"])
        .arg(patch_path)
        .current_dir(component_path)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to inspect trace overlay {}: {}",
                    patch_path.display(),
                    e
                ),
                Some("trace.overlay.inspect".to_string()),
            )
        })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "--overlay",
            format!("trace overlay {} cannot be inspected", patch_path.display()),
            Some(String::from_utf8_lossy(&output.stderr).to_string()),
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split('\t').nth(2))
        .map(unquote_numstat_path)
        .filter(|path| !path.is_empty())
        .collect())
}

fn ensure_overlay_targets_clean(
    component_path: &Path,
    patch_path: &Path,
    touched_files: &[String],
) -> Result<()> {
    if touched_files.is_empty() {
        return Ok(());
    }
    let mut command = Command::new("git");
    command
        .args(["status", "--porcelain=v1", "--"])
        .args(touched_files)
        .current_dir(component_path);
    let output = command.output().map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to check trace overlay targets for {}: {}",
                patch_path.display(),
                e
            ),
            Some("trace.overlay.status".to_string()),
        )
    })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "--overlay",
            format!(
                "failed to check overlay target status for {}",
                patch_path.display()
            ),
            Some(String::from_utf8_lossy(&output.stderr).to_string()),
            None,
        ));
    }
    let dirty = String::from_utf8_lossy(&output.stdout);
    if !dirty.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "--overlay",
            format!(
                "trace overlay {} touches files with pre-existing changes",
                patch_path.display()
            ),
            Some(dirty.to_string()),
            None,
        ));
    }
    Ok(())
}

fn run_git_apply(component_path: &Path, patch_path: &Path, reverse: bool) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("apply");
    if reverse {
        command.arg("--reverse");
    }
    let output = command
        .arg(patch_path)
        .current_dir(component_path)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to apply trace overlay {}: {}",
                    patch_path.display(),
                    e
                ),
                Some("trace.overlay.apply".to_string()),
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    let action = if reverse { "revert" } else { "apply" };
    Err(Error::validation_invalid_argument(
        "--overlay",
        format!(
            "failed to {} trace overlay {}",
            action,
            patch_path.display()
        ),
        Some(String::from_utf8_lossy(&output.stderr).to_string()),
        None,
    ))
}

fn unquote_numstat_path(path: &str) -> String {
    path.trim().trim_matches('"').to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use crate::core::engine::run_dir::RunDir;
    use crate::test_support::with_isolated_home;

    use super::*;

    #[test]
    fn test_acquire_trace_overlay_locks() {
        with_isolated_home(|_| {
            let run_dir = RunDir::create().unwrap();
            let first = overlay_fixture("first");
            let second = overlay_fixture("second");

            let locks = acquire_trace_overlay_locks(
                &[first.request.clone(), second.request.clone()],
                &run_dir,
            )
            .unwrap();

            assert_eq!(locks.len(), 2);
            run_dir.cleanup();
        });
    }

    #[test]
    fn test_apply_trace_overlays() {
        let fixture = overlay_fixture("overlay");
        let applied = apply_trace_overlays(&[fixture.request.clone()], true).unwrap();

        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].touched_files, vec!["scenario.txt"]);
        assert_eq!(
            fs::read_to_string(fixture.component_dir.join("scenario.txt")).unwrap(),
            "overlay\n"
        );
    }

    #[test]
    fn test_cleanup_trace_overlays() {
        let fixture = overlay_fixture("overlay");
        let applied = apply_trace_overlays(&[fixture.request.clone()], true).unwrap();

        cleanup_trace_overlays(&applied).unwrap();

        assert_eq!(
            fs::read_to_string(fixture.component_dir.join("scenario.txt")).unwrap(),
            "base\n"
        );
    }

    #[test]
    fn test_overlay_touched_files() {
        let fixture = overlay_fixture("overlay");

        let touched = overlay_touched_files(
            &fixture.component_dir,
            Path::new(&fixture.request.overlay_path),
        )
        .unwrap();

        assert_eq!(touched, vec!["scenario.txt"]);
    }

    struct OverlayFixture {
        _temp: tempfile::TempDir,
        component_dir: PathBuf,
        request: TraceOverlayRequest,
    }

    fn overlay_fixture(replacement: &str) -> OverlayFixture {
        let temp = tempfile::tempdir().unwrap();
        let component_dir = temp.path().join("component");
        fs::create_dir_all(&component_dir).unwrap();
        fs::write(component_dir.join("scenario.txt"), "base\n").unwrap();
        init_git_repo(&component_dir);
        let patch_path = temp.path().join("overlay.patch");
        fs::write(
            &patch_path,
            format!(
                r#"--- a/scenario.txt
+++ b/scenario.txt
@@ -1 +1 @@
-base
+{replacement}
"#
            ),
        )
        .unwrap();

        OverlayFixture {
            _temp: temp,
            component_dir: component_dir.clone(),
            request: TraceOverlayRequest {
                variant: None,
                component_id: Some("component".to_string()),
                component_path: component_dir.to_string_lossy().to_string(),
                overlay_path: patch_path.to_string_lossy().to_string(),
            },
        }
    }

    fn init_git_repo(path: &Path) {
        git(path, &["init"]);
        git(path, &["add", "scenario.txt"]);
        git(
            path,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "init",
            ],
        );
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
