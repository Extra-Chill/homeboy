//! Changed-file scope resolution — maps `--changed-only`/`--changed-since`
//! flags into runner-compatible globbed lint runs via extension routes.

use super::types::{LintRunWorkflowArgs, ScopedLintRun};
use crate::core::component::Component;
use crate::core::extension::{self, LintChangedFileRoute};
use crate::core::git;
use std::path::Path;

/// Resolve runner-compatible scopes from --changed-only or --changed-since flags.
///
/// Returns `Some(Vec::new())` when changed-file mode is active but no compatible
/// files were found — the caller should treat this as an early "passed" exit.
/// Returns `None` when no changed-file scoping is active (use args.glob directly).
pub(super) fn resolve_scoped_lint_runs(
    component: &Component,
    args: &LintRunWorkflowArgs,
) -> crate::core::Result<Option<Vec<ScopedLintRun>>> {
    if args.changed_only {
        let changed_files = if let Some(files) = &args.precomputed_changed_files {
            files.clone()
        } else {
            let uncommitted = git::get_uncommitted_changes(&component.local_path)?;
            let mut files: Vec<String> = Vec::new();
            files.extend(uncommitted.staged);
            files.extend(uncommitted.unstaged);
            files.extend(uncommitted.untracked);
            files
        };

        if changed_files.is_empty() {
            println!("No files in working tree changes");
            return Ok(Some(Vec::new()));
        }

        eprintln!(
            "Linting {} changed file(s) (--changed-only is file-scoped; findings may be outside changed hunks)",
            changed_files.len()
        );

        Ok(Some(build_changed_lint_runs(component, &changed_files)))
    } else if let Some(ref git_ref) = args.changed_since {
        let changed_files = match &args.precomputed_changed_files {
            Some(files) => files.clone(),
            None => git::get_files_changed_since(&component.local_path, git_ref)?,
        };

        if changed_files.is_empty() {
            println!("No files changed since {}", git_ref);
            return Ok(Some(Vec::new()));
        }

        Ok(Some(build_changed_lint_runs(component, &changed_files)))
    } else {
        Ok(None)
    }
}

pub(super) fn build_changed_lint_runs(
    component: &Component,
    changed_files: &[String],
) -> Vec<ScopedLintRun> {
    let routes = changed_file_routes_for_component(component);
    build_changed_lint_runs_with_routes(component, changed_files, &routes)
}

pub(super) fn build_changed_lint_runs_with_routes(
    component: &Component,
    changed_files: &[String],
    routes: &[LintChangedFileRoute],
) -> Vec<ScopedLintRun> {
    if routes.is_empty() {
        return vec![ScopedLintRun {
            glob: glob_for_files(&component.local_path, changed_files),
            step: None,
            changed_files: changed_files.to_vec(),
        }];
    }

    let mut runs = Vec::new();
    for route in routes {
        let matched_files: Vec<String> = changed_files
            .iter()
            .filter(|file| route_matches_file(route, file))
            .cloned()
            .collect();

        if !matched_files.is_empty() {
            runs.push(ScopedLintRun {
                glob: glob_for_files(&component.local_path, &matched_files),
                step: Some(route.step.clone()),
                changed_files: matched_files,
            });
        }
    }
    runs
}

fn changed_file_routes_for_component(component: &Component) -> Vec<LintChangedFileRoute> {
    let Some(extensions) = component.extensions.as_ref() else {
        return Vec::new();
    };

    extensions
        .keys()
        .filter_map(|extension_id| extension::load_extension(extension_id).ok())
        .filter_map(|manifest| manifest.lint)
        .flat_map(|lint| lint.changed_file_routes)
        .collect()
}

fn route_matches_file(route: &LintChangedFileRoute, file: &str) -> bool {
    if !route.extensions.is_empty() && has_extension(file, &route.extensions) {
        return true;
    }

    route
        .globs
        .iter()
        .any(|pattern| glob_match::glob_match(pattern, file))
}

fn has_extension(file: &str, extensions: &[String]) -> bool {
    Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extensions.iter().any(|expected| expected == extension))
}

fn glob_for_files(root: &str, files: &[String]) -> String {
    let abs_files: Vec<String> = files
        .iter()
        .map(|file| format!("{}/{}", root, file))
        .collect();

    if abs_files.len() == 1 {
        abs_files[0].clone()
    } else {
        format!("{{{}}}", abs_files.join(","))
    }
}
