pub mod baseline;
pub mod report;
pub mod run;

use crate::core::component::Component;
use crate::core::extension::{ExtensionCapability, ExtensionExecutionContext, ExtensionRunner};

pub use baseline::{BaselineComparison, LintBaseline, LintBaselineMetadata};
pub use report::LintCommandOutput;
pub use run::{
    run_main_lint_workflow, run_self_check_lint_workflow,
    run_self_check_lint_workflow_with_progress, LintRunWorkflowArgs, LintRunWorkflowResult,
    LintSniffFilters,
};

use crate::core::engine::run_dir::RunDir;

pub fn resolve_lint_command(
    component: &Component,
) -> crate::core::error::Result<ExtensionExecutionContext> {
    crate::core::extension::resolve_execution_context(component, ExtensionCapability::Lint)
}

#[allow(clippy::too_many_arguments)]
pub fn build_lint_runner(
    component: &Component,
    path_override: Option<String>,
    settings: &[(String, serde_json::Value)],
    summary: bool,
    file: Option<&str>,
    glob: Option<&str>,
    errors_only: bool,
    sniffs: Option<&str>,
    exclude_sniffs: Option<&str>,
    category: Option<&str>,
    step: Option<&str>,
    changed_files: Option<&[String]>,
    run_dir: &RunDir,
) -> crate::core::Result<ExtensionRunner> {
    let resolved = resolve_lint_command(component)?;
    let (string_settings, json_settings) = split_lint_settings(settings);

    // Additive: hand the runner core's authoritative changed-file list, resolved
    // via the three-dot merge-base diff in `get_files_changed_since`. Downstream
    // lint runners can consume `HOMEBOY_LINT_CHANGED_FILES_FILE` instead of
    // re-deriving a weaker two-dot diff. Leaves the var unset for full/glob runs,
    // so existing `HOMEBOY_LINT_GLOB`/`HOMEBOY_LINT_FILE`/`HOMEBOY_CHANGED_SINCE`
    // semantics are untouched.
    let changed_files_file = write_changed_files_manifest(run_dir, changed_files)?;

    Ok(ExtensionRunner::for_context(resolved)
        .component(component.clone())
        .path_override(path_override)
        .settings(&string_settings)
        .settings_json(&json_settings)
        .with_run_dir(run_dir)
        .env_if(summary, "HOMEBOY_SUMMARY_MODE", "1")
        .env_opt("HOMEBOY_LINT_FILE", &file.map(str::to_string))
        .env_opt("HOMEBOY_LINT_GLOB", &glob.map(str::to_string))
        .env_if(errors_only, "HOMEBOY_ERRORS_ONLY", "1")
        .env_opt("HOMEBOY_SNIFFS", &sniffs.map(str::to_string))
        .env_opt("HOMEBOY_STEP", &step.map(str::to_string))
        .env_opt(
            "HOMEBOY_EXCLUDE_SNIFFS",
            &exclude_sniffs.map(str::to_string),
        )
        .env_opt("HOMEBOY_CATEGORY", &category.map(str::to_string))
        .env_opt(
            "HOMEBOY_LINT_CHANGED_FILES_FILE",
            &changed_files_file,
        ))
}

/// Write core's resolved changed-file list to a newline-delimited manifest in
/// the run dir, returning its path for `HOMEBOY_LINT_CHANGED_FILES_FILE`.
///
/// Returns `Ok(None)` when no changed-file list is supplied or it is empty,
/// leaving the env var unset so full/glob lint runs are unaffected. The paths
/// are written exactly as core resolved them (repo-relative, via the three-dot
/// merge-base diff in `get_files_changed_since`), one per line.
fn write_changed_files_manifest(
    run_dir: &RunDir,
    changed_files: Option<&[String]>,
) -> crate::core::Result<Option<String>> {
    let Some(changed_files) = changed_files.filter(|files| !files.is_empty()) else {
        return Ok(None);
    };

    let manifest_path =
        run_dir.step_file(crate::core::engine::run_dir::files::LINT_CHANGED_FILES);
    let mut contents = changed_files.join("\n");
    contents.push('\n');
    std::fs::write(&manifest_path, contents).map_err(|error| {
        crate::core::error::Error::internal_io(
            error.to_string(),
            Some("write lint changed-files manifest".to_string()),
        )
    })?;

    Ok(Some(manifest_path.to_string_lossy().to_string()))
}

fn split_lint_settings(
    settings: &[(String, serde_json::Value)],
) -> (Vec<(String, String)>, Vec<(String, serde_json::Value)>) {
    settings
        .iter()
        .cloned()
        .fold((Vec::new(), Vec::new()), |mut split, (key, value)| {
            match value {
                serde_json::Value::String(value) => split.0.push((key, value)),
                value => split.1.push((key, value)),
            }
            split
        })
}

pub(crate) fn settings_from_legacy_strings(
    settings: &[(String, String)],
) -> Vec<(String, serde_json::Value)> {
    settings
        .iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_lint_settings_preserves_typed_json_values() {
        let settings = vec![
            (
                "severity".to_string(),
                serde_json::Value::String("strict".to_string()),
            ),
            ("rules".to_string(), json!({ "security": true })),
        ];

        let (string_settings, json_settings) = split_lint_settings(&settings);

        assert_eq!(
            string_settings,
            vec![("severity".to_string(), "strict".to_string())]
        );
        assert_eq!(
            json_settings,
            vec![("rules".to_string(), json!({ "security": true }))]
        );
    }

    #[test]
    fn legacy_lint_settings_are_explicitly_wrapped_as_strings() {
        let settings = settings_from_legacy_strings(&[("mode".to_string(), "strict".to_string())]);

        assert_eq!(
            settings,
            vec![(
                "mode".to_string(),
                serde_json::Value::String("strict".to_string()),
            ),]
        );
    }

    #[test]
    fn changed_files_manifest_unset_when_no_list_supplied() {
        let run_dir = RunDir::create().expect("run dir");

        assert_eq!(write_changed_files_manifest(&run_dir, None).expect("none"), None);
        assert_eq!(
            write_changed_files_manifest(&run_dir, Some(&[])).expect("empty"),
            None
        );

        assert!(
            !run_dir
                .step_file(crate::core::engine::run_dir::files::LINT_CHANGED_FILES)
                .exists(),
            "no manifest is written when there is no changed-file list"
        );
    }

    /// The manifest must carry core's authoritative changed-file list verbatim
    /// (one repo-relative path per line) so downstream runners consume it
    /// instead of re-deriving a weaker two-dot diff.
    #[test]
    fn changed_files_manifest_matches_resolved_list() {
        let run_dir = RunDir::create().expect("run dir");
        let changed_files = vec![
            "inc/Foo.php".to_string(),
            "assets/app.js".to_string(),
            "README.md".to_string(),
        ];

        let manifest = write_changed_files_manifest(&run_dir, Some(&changed_files))
            .expect("manifest write")
            .expect("manifest path");

        let contents = std::fs::read_to_string(&manifest).expect("read manifest");
        assert_eq!(contents, "inc/Foo.php\nassets/app.js\nREADME.md\n");

        let lines: Vec<String> = contents.lines().map(str::to_string).collect();
        assert_eq!(lines, changed_files);
    }

    /// End-to-end: the manifest content equals `get_files_changed_since`, the
    /// three-dot merge-base resolution that core owns. Proves the injected list
    /// is the authoritative one, not a re-derivation.
    #[test]
    fn changed_files_manifest_round_trips_get_files_changed_since() {
        use std::fs;
        use std::process::Command;
        use tempfile::TempDir;

        let repo = TempDir::new().expect("tempdir");
        let path = repo.path().to_str().unwrap();

        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .output();
        if init.is_err() || !init.unwrap().status.success() {
            // Test environment lacks git — skip rather than fail.
            return;
        }
        for args in [
            ["config", "user.email", "test@example.com"],
            ["config", "user.name", "test"],
        ] {
            let _ = Command::new("git").args(args).current_dir(path).output();
        }

        fs::write(repo.path().join("tracked.txt"), "initial\n").expect("write tracked");
        let _ = Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output();
        let _ = Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(path)
            .output();

        fs::write(repo.path().join("tracked.txt"), "dirty\n").expect("modify tracked");
        fs::write(repo.path().join("untracked.txt"), "new\n").expect("write untracked");

        let resolved =
            crate::core::git::get_files_changed_since(path, "HEAD").expect("changed files");
        assert!(
            !resolved.is_empty(),
            "expected git to report changed files: {resolved:?}"
        );

        let run_dir = RunDir::create().expect("run dir");
        let manifest = write_changed_files_manifest(&run_dir, Some(&resolved))
            .expect("manifest write")
            .expect("manifest path");

        let contents = std::fs::read_to_string(&manifest).expect("read manifest");
        let manifest_files: Vec<String> = contents.lines().map(str::to_string).collect();
        assert_eq!(
            manifest_files, resolved,
            "manifest must match get_files_changed_since verbatim"
        );
    }
}
