pub mod baseline;
pub mod report;
pub mod run;

use crate::extension::ExtensionRunner;
use crate::extension_execution::ExtensionExecutionContext;
use homeboy_core::component::Component;
use homeboy_extension_contract::ExtensionCapability;

pub use baseline::{BaselineComparison, LintBaseline, LintBaselineMetadata};
pub use report::LintCommandOutput;
pub use run::{
    resolve_lint_fix_routes, run_main_lint_workflow, run_self_check_lint_workflow,
    run_self_check_lint_workflow_with_progress, LintFixRoute, LintRunWorkflowArgs,
    LintRunWorkflowResult, LintSniffFilters,
};

use homeboy_core::engine::run_dir::RunDir;

pub type LintStringSettings = Vec<(String, String)>;
pub type LintJsonSettings = Vec<(String, serde_json::Value)>;

pub struct LintRunnerRequest<'a> {
    pub component: &'a Component,
    pub path_override: Option<String>,
    pub settings: &'a [(String, serde_json::Value)],
    pub summary: bool,
    pub file: Option<&'a str>,
    pub glob: Option<&'a str>,
    pub errors_only: bool,
    pub sniffs: Option<&'a str>,
    pub exclude_sniffs: Option<&'a str>,
    pub category: Option<&'a str>,
    pub step: Option<&'a str>,
    pub changed_files: Option<&'a [String]>,
    pub run_dir: &'a RunDir,
}

pub fn resolve_lint_command(
    component: &Component,
) -> homeboy_core::error::Result<ExtensionExecutionContext> {
    crate::extension_execution::resolve_execution_context(component, ExtensionCapability::Lint)
}

/// The manifest key for the lint findings structured sidecar.
pub const LINT_FINDINGS_SIDECAR: &str = "lint.findings";

/// Whether the extension providing this component's lint capability declares a
/// `lint.findings` structured sidecar.
///
/// This is what makes the missing-findings evidence check legitimate. That
/// check hard-fails a lint run with an `internal.io_error` — an *infrastructure*
/// error, not a lint result — when the sidecar file is absent. Demanding a file
/// from an extension that never declared it produced exactly that error on
/// runs whose lint had *passed*, which is the outage class #11123 retires.
///
/// An extension with no declaration has no contract to hold it to, so the check
/// is skipped and a missing file reads as zero findings, which is what
/// `lint::baseline::parse_findings_file` already returns for an absent file.
///
/// Failing to resolve or load the manifest yields `false`. By the time this is
/// consulted the lint runner has already resolved and executed against that
/// same manifest, so a failure here is not a real state; treating it as "no
/// declaration" keeps a bookkeeping hiccup from being reported as a lint
/// verdict.
pub fn declares_lint_findings_sidecar(component: &Component) -> bool {
    let Ok(context) = resolve_lint_command(component) else {
        return false;
    };
    let Ok(manifest) = crate::extension::catalog::load_extension(&context.extension_id) else {
        return false;
    };
    crate::extension::structured_sidecars(&manifest)
        .iter()
        .any(|declaration| declaration.name == LINT_FINDINGS_SIDECAR)
}

pub fn build_lint_runner(request: LintRunnerRequest<'_>) -> homeboy_core::Result<ExtensionRunner> {
    let resolved = resolve_lint_command(request.component)?;
    let (string_settings, json_settings) = split_lint_settings(request.settings);

    // Additive: hand the runner core's authoritative changed-file list, resolved
    // via the three-dot merge-base diff in `get_files_changed_since`. Downstream
    // lint runners can consume `HOMEBOY_LINT_CHANGED_FILES_FILE` instead of
    // re-deriving a weaker two-dot diff. Leaves the var unset for full/glob runs,
    // so existing `HOMEBOY_LINT_GLOB`/`HOMEBOY_LINT_FILE`/`HOMEBOY_CHANGED_SINCE`
    // semantics are untouched.
    let changed_files_file = write_changed_files_manifest(request.run_dir, request.changed_files)?;

    Ok(ExtensionRunner::for_context(resolved)
        .component(request.component.clone())
        .path_override(request.path_override)
        .settings(&string_settings)
        .settings_json(&json_settings)
        .with_run_dir(request.run_dir)
        // Summary mode captures the lint/clippy stream into evidence rather than
        // tee-ing the full warning output to the terminal, so `--summary` renders
        // only the compact findings envelope on large repositories (#9845).
        .passthrough(!request.summary)
        .env_if(request.summary, "HOMEBOY_SUMMARY_MODE", "1")
        .env_opt("HOMEBOY_LINT_FILE", &request.file.map(str::to_string))
        .env_opt("HOMEBOY_LINT_GLOB", &request.glob.map(str::to_string))
        .env_if(request.errors_only, "HOMEBOY_ERRORS_ONLY", "1")
        .env_opt("HOMEBOY_SNIFFS", &request.sniffs.map(str::to_string))
        .env_opt("HOMEBOY_STEP", &request.step.map(str::to_string))
        .env_opt(
            "HOMEBOY_EXCLUDE_SNIFFS",
            &request.exclude_sniffs.map(str::to_string),
        )
        .env_opt("HOMEBOY_CATEGORY", &request.category.map(str::to_string))
        .env_opt("HOMEBOY_LINT_CHANGED_FILES_FILE", &changed_files_file))
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
) -> homeboy_core::Result<Option<String>> {
    let Some(changed_files) = changed_files.filter(|files| !files.is_empty()) else {
        return Ok(None);
    };

    let manifest_path = run_dir.step_file(homeboy_core::engine::run_dir::files::LINT_CHANGED_FILES);
    let mut contents = changed_files.join("\n");
    contents.push('\n');
    std::fs::write(&manifest_path, contents).map_err(|error| {
        homeboy_core::error::Error::internal_io(
            error.to_string(),
            Some("write lint changed-files manifest".to_string()),
        )
    })?;

    Ok(Some(manifest_path.to_string_lossy().to_string()))
}

fn split_lint_settings(
    settings: &[(String, serde_json::Value)],
) -> (LintStringSettings, LintJsonSettings) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::component::{Component, ScopedExtensionConfig};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn summary_runner_captures_streams_for_evidence() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extension = home.path().join(".config/homeboy/extensions/fixture");
            std::fs::create_dir_all(&extension).expect("extension directory");
            std::fs::write(
                extension.join("fixture.json"),
                r#"{"name":"fixture","version":"1.0.0","lint":{"extension_script":"lint.sh"}}"#,
            )
            .expect("extension manifest");
            let component = Component {
                id: "fixture".to_string(),
                extensions: Some(HashMap::from([(
                    "fixture".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };
            let run_dir = RunDir::create().expect("run dir");

            let summary = build_lint_runner(LintRunnerRequest {
                component: &component,
                path_override: None,
                settings: &[],
                summary: true,
                file: None,
                glob: None,
                errors_only: false,
                sniffs: None,
                exclude_sniffs: None,
                category: None,
                step: None,
                changed_files: None,
                run_dir: &run_dir,
            })
            .expect("summary runner");

            assert!(
                !summary.is_passthrough(),
                "summary mode captures full child output in run evidence"
            );
        });
    }

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
    fn changed_files_manifest_unset_when_no_list_supplied() {
        let run_dir = RunDir::create().expect("run dir");

        assert_eq!(
            write_changed_files_manifest(&run_dir, None).expect("none"),
            None
        );
        assert_eq!(
            write_changed_files_manifest(&run_dir, Some(&[])).expect("empty"),
            None
        );

        assert!(
            !run_dir
                .step_file(homeboy_core::engine::run_dir::files::LINT_CHANGED_FILES)
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

    /// The missing-findings evidence check is only legitimate against an
    /// extension that declared `lint.findings`. Before #11123 it consulted no
    /// declaration at all, so an extension declaring `"lint.findings": false`
    /// — or declaring nothing — still got a hard `internal.io_error` on a
    /// *passing* lint. (#11123)
    #[test]
    fn lint_findings_evidence_is_required_only_when_declared() {
        fn write_lint_extension(home: &std::path::Path, extension_id: &str, sidecars: &str) {
            let extension_dir = home.join(".config/homeboy/extensions").join(extension_id);
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            std::fs::write(
                extension_dir.join(format!("{extension_id}.json")),
                format!(
                    r#"{{"name":"{extension_id}","version":"1.0.0","lint":{{"extension_script":"lint.sh"}}{sidecars}}}"#
                ),
            )
            .expect("extension manifest");
        }

        fn component_for(extension_id: &str) -> Component {
            Component {
                id: "consumer".to_string(),
                extensions: Some(std::collections::HashMap::from([(
                    extension_id.to_string(),
                    homeboy_core::component::ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            }
        }

        homeboy_core::test_support::with_isolated_home(|home| {
            write_lint_extension(
                home.path(),
                "declares",
                r#","structured_sidecars":{"lint.findings":true}"#,
            );
            write_lint_extension(
                home.path(),
                "disclaims",
                r#","structured_sidecars":{"lint.findings":false}"#,
            );
            write_lint_extension(home.path(), "silent", "");

            assert!(
                declares_lint_findings_sidecar(&component_for("declares")),
                "an extension that declares the sidecar owes it on every exit path"
            );
            assert!(
                !declares_lint_findings_sidecar(&component_for("disclaims")),
                "`\"lint.findings\": false` must not be answered with a hard IO error"
            );
            assert!(
                !declares_lint_findings_sidecar(&component_for("silent")),
                "no declaration means no contract to enforce"
            );
            assert!(
                !declares_lint_findings_sidecar(&component_for("missing")),
                "an unresolvable lint capability is not an evidence verdict"
            );
        });
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
            homeboy_core::git::get_files_changed_since(path, "HEAD").expect("changed files");
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
