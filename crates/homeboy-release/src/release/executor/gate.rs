//! Declared release gates (`release.gates` in `homeboy.json`).
//!
//! A gate is a component-declared command run from the component root against
//! the exact artifact the release will publish — after packaging, before any
//! tag or publication. A non-zero exit fails the release with the gate id and
//! the tail of its output, exactly the way a failing test does.

use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy_core::component::{Component, ReleaseGateConfig};
use homeboy_core::error::{Error, Result};

use super::{step_failed, step_success};
use crate::release::types::{ReleaseArtifact, ReleaseState, ReleaseStepResult};

/// Step kind shared by the planner and the execution dispatcher.
pub(crate) const RELEASE_GATE_STEP_KIND: &str = "release.gate";

/// Plan/result step id for one declared gate.
pub(crate) fn release_gate_step_id(gate_id: &str) -> String {
    format!("{}.{}", RELEASE_GATE_STEP_KIND, gate_id)
}

/// Run one declared release gate against the packaged artifacts.
///
/// The gate command runs from the component root with `{artifact}` replaced by
/// the shell-quoted path of the selected artifact. When the gate declares an
/// `evidence` directory and the gate wrote it, the directory is persisted into
/// the durable release artifact store regardless of the gate's outcome, so a
/// failing gate still leaves its evidence behind.
pub(crate) fn run_release_gate(
    roots: &homeboy_core::paths::PathRoots,
    component: &Component,
    state: &ReleaseState,
    gate: &ReleaseGateConfig,
) -> Result<ReleaseStepResult> {
    let step_id = release_gate_step_id(&gate.id);
    let component_path = Path::new(&component.local_path);

    let artifact = match select_gate_artifact(&state.artifacts, gate) {
        Ok(artifact) => artifact,
        Err(err) => {
            return Ok(step_failed(
                &step_id,
                RELEASE_GATE_STEP_KIND,
                Some(serde_json::json!({
                    "gate": gate.id,
                    "artifacts": state
                        .artifacts
                        .iter()
                        .map(|artifact| artifact.path.clone())
                        .collect::<Vec<_>>(),
                })),
                Some(err.message),
                err.hints,
            ));
        }
    };

    let artifact_path = match resolve_gate_artifact_path(component_path, artifact) {
        Ok(path) => path,
        Err(err) => {
            return Ok(step_failed(
                &step_id,
                RELEASE_GATE_STEP_KIND,
                Some(serde_json::json!({ "gate": gate.id, "artifact": artifact.path })),
                Some(err.message),
                err.hints,
            ));
        }
    };

    let command = gate.run.replace(
        "{artifact}",
        &shell_quote(&artifact_path.display().to_string()),
    );

    let output = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .current_dir(component_path)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to run release gate '{}': {}", gate.id, e),
                Some(command.clone()),
            )
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    // Evidence is persisted for passing and failing gates alike: a failing
    // gate's evidence is exactly what the operator needs to diagnose the block.
    let evidence = persist_gate_evidence(roots, component, state, gate)?;

    let mut data = serde_json::json!({
        "gate": gate.id,
        "command": command,
        "artifact": artifact_path.display().to_string(),
        "exit_code": exit_code,
        "stdout_tail": output_tail(&stdout, GATE_OUTPUT_TAIL_BYTES),
        "stderr_tail": output_tail(&stderr, GATE_OUTPUT_TAIL_BYTES),
    });
    if let Some(evidence) = evidence.as_ref() {
        data["evidence"] = serde_json::json!(evidence);
    }

    if output.status.success() {
        return Ok(step_success(
            &step_id,
            RELEASE_GATE_STEP_KIND,
            Some(data),
            Vec::new(),
        ));
    }

    let tail = output_tail(
        &format!("{}\n{}", stdout.trim_end(), stderr.trim_end()),
        GATE_OUTPUT_TAIL_BYTES,
    );
    let error = format!(
        "Release gate '{}' failed (exit {}). The release stops before any tag.\n{}",
        gate.id,
        exit_code,
        tail.trim()
    );

    Ok(step_failed(
        &step_id,
        RELEASE_GATE_STEP_KIND,
        Some(data),
        Some(error),
        Vec::new(),
    ))
}

const GATE_OUTPUT_TAIL_BYTES: usize = 4096;

/// Select the artifact a gate receives.
///
/// Without a selector the gate gets the release's primary artifact: the first
/// artifact holding publication authority, falling back to the first artifact.
/// A `path` selector matches one artifact's declared path exactly; a `profile`
/// selector matches built artifact file names containing the profile and must
/// select exactly one.
fn select_gate_artifact<'a>(
    artifacts: &'a [ReleaseArtifact],
    gate: &ReleaseGateConfig,
) -> Result<&'a ReleaseArtifact> {
    if artifacts.is_empty() {
        return Err(Error::validation_invalid_argument(
            "release.gates",
            format!(
                "Release gate '{}' has no packaged artifacts to run against",
                gate.id
            ),
            None,
            Some(vec![
                "Release gates run after packaging; declare a package contract (release.package extension or build_artifact) so the gate receives an artifact.".to_string(),
            ]),
        ));
    }

    let Some(selector) = gate.artifact.as_ref() else {
        return Ok(artifacts
            .iter()
            .find(|artifact| artifact.publication_authority)
            .unwrap_or(&artifacts[0]));
    };

    if let Some(path) = selector
        .path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
    {
        let normalized = normalize_path(path);
        return artifacts
            .iter()
            .find(|artifact| normalize_path(&artifact.path) == normalized)
            .ok_or_else(|| {
                gate_artifact_selection_error(gate, artifacts, &format!("path '{path}'"))
            });
    }

    if let Some(profile) = selector
        .profile
        .as_deref()
        .filter(|profile| !profile.trim().is_empty())
    {
        let matches: Vec<&ReleaseArtifact> = artifacts
            .iter()
            .filter(|artifact| artifact_file_name(&artifact.path).contains(profile))
            .collect();
        return match matches.as_slice() {
            [artifact] => Ok(artifact),
            [] => Err(gate_artifact_selection_error(
                gate,
                artifacts,
                &format!("profile '{profile}' (no artifact file name contains it)"),
            )),
            _ => Err(gate_artifact_selection_error(
                gate,
                artifacts,
                &format!(
                    "profile '{profile}' (matched {} artifacts; it must match exactly one)",
                    matches.len()
                ),
            )),
        };
    }

    Err(Error::validation_invalid_argument(
        "release.gates",
        format!(
            "Release gate '{}' declares an artifact selector without a profile or path",
            gate.id
        ),
        None,
        None,
    ))
}

fn gate_artifact_selection_error(
    gate: &ReleaseGateConfig,
    artifacts: &[ReleaseArtifact],
    selector: &str,
) -> Error {
    Error::validation_invalid_argument(
        "release.gates",
        format!(
            "Release gate '{}' artifact selector {} selected no unique artifact. Built artifacts: {}",
            gate.id,
            selector,
            artifacts
                .iter()
                .map(|artifact| artifact.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None,
        None,
    )
}

/// Resolve the on-disk path handed to the gate. The checkout copy is preferred
/// (gates run from the component root before cleanup); the durable copy is the
/// fallback when the checkout copy is already gone.
fn resolve_gate_artifact_path(
    component_path: &Path,
    artifact: &ReleaseArtifact,
) -> Result<PathBuf> {
    let source = resolve_relative(component_path, &artifact.path);
    if source.is_file() {
        return Ok(source);
    }
    if let Some(durable) = artifact
        .durable_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
    {
        let durable = resolve_relative(component_path, durable);
        if durable.is_file() {
            return Ok(durable);
        }
    }
    Err(Error::internal_io(
        format!(
            "Release gate artifact '{}' does not exist on disk",
            artifact.path
        ),
        Some(source.display().to_string()),
    ))
}

fn resolve_relative(component_path: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        component_path.join(path)
    }
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

fn artifact_file_name(path: &str) -> String {
    normalize_path(path)
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// POSIX shell single-quote the artifact path for `{artifact}` substitution.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Last `max_bytes` of a gate's output, trimmed to a char boundary.
fn output_tail(output: &str, max_bytes: usize) -> String {
    let trimmed = output.trim_end();
    if trimmed.len() <= max_bytes {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - max_bytes;
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &trimmed[start..])
}

/// Copy the gate's declared evidence directory into the durable release
/// artifact store (`<artifacts>/release/<component>/<version>/gates/<gate>`).
///
/// Returns the durable evidence directory when evidence was written, `None`
/// when the gate declared none or wrote nothing.
fn persist_gate_evidence(
    roots: &homeboy_core::paths::PathRoots,
    component: &Component,
    state: &ReleaseState,
    gate: &ReleaseGateConfig,
) -> Result<Option<String>> {
    let Some(evidence) = gate
        .evidence
        .as_deref()
        .filter(|dir| !dir.trim().is_empty())
    else {
        return Ok(None);
    };

    let source = Path::new(&component.local_path).join(evidence);
    if !source.is_dir() {
        return Ok(None);
    }

    let version = state
        .version
        .as_deref()
        .filter(|version| !version.trim().is_empty())
        .unwrap_or("unversioned");
    let target = roots
        .artifacts()
        .join("release")
        .join(homeboy_core::paths::sanitize_path_segment(&component.id))
        .join(homeboy_core::paths::sanitize_path_segment(version))
        .join("gates")
        .join(homeboy_core::paths::sanitize_path_segment(&gate.id));

    copy_dir_recursive(&source, &target).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to persist release gate '{}' evidence: {}",
                gate.id, e
            ),
            Some(format!("{} -> {}", source.display(), target.display())),
        )
    })?;

    Ok(Some(target.display().to_string()))
}

fn copy_dir_recursive(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &destination)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::types::ReleaseStepStatus;
    use homeboy_core::component::ReleaseGateArtifactSelector;

    fn artifact(path: &str, authority: bool) -> ReleaseArtifact {
        ReleaseArtifact {
            path: path.to_string(),
            durable_path: None,
            artifact_type: None,
            platform: None,
            phase: "final".to_string(),
            producer: "test".to_string(),
            sha256: None,
            publication_authority: authority,
        }
    }

    fn gate(id: &str, run: &str) -> ReleaseGateConfig {
        ReleaseGateConfig {
            id: id.to_string(),
            run: run.to_string(),
            artifact: None,
            evidence: None,
        }
    }

    fn test_roots() -> homeboy_core::paths::PathRoots {
        homeboy_core::paths::PathRoots::from_environment().expect("path roots")
    }

    fn state_with_artifact(path: &str) -> ReleaseState {
        ReleaseState {
            version: Some("1.2.3".to_string()),
            artifacts: vec![artifact(path, true)],
            ..ReleaseState::default()
        }
    }

    fn component_at(dir: &Path) -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            ..Component::default()
        }
    }

    #[test]
    fn gate_without_selector_receives_primary_artifact() {
        let artifacts = vec![
            artifact("build/first.zip", false),
            artifact("build/primary.zip", true),
        ];
        let selected = select_gate_artifact(&artifacts, &gate("g", "true")).expect("artifact");
        assert_eq!(selected.path, "build/primary.zip");
    }

    #[test]
    fn gate_profile_selector_must_match_exactly_one_artifact() {
        let artifacts = vec![
            artifact("build/site-html-site-import.zip", false),
            artifact("build/site-full.zip", true),
        ];
        let mut gate = gate("import", "true");
        gate.artifact = Some(ReleaseGateArtifactSelector {
            profile: Some("html-site-import".to_string()),
            path: None,
        });
        let selected = select_gate_artifact(&artifacts, &gate).expect("artifact");
        assert_eq!(selected.path, "build/site-html-site-import.zip");

        gate.artifact = Some(ReleaseGateArtifactSelector {
            profile: Some("site".to_string()),
            path: None,
        });
        let err = select_gate_artifact(&artifacts, &gate).expect_err("ambiguous profile");
        assert!(err.message.contains("must match exactly one"));

        gate.artifact = Some(ReleaseGateArtifactSelector {
            profile: Some("missing-profile".to_string()),
            path: None,
        });
        let err = select_gate_artifact(&artifacts, &gate).expect_err("unmatched profile");
        assert!(err.message.contains("no artifact file name contains it"));
    }

    #[test]
    fn gate_path_selector_matches_declared_artifact_path() {
        let artifacts = vec![
            artifact("build/first.zip", true),
            artifact("build/second.zip", false),
        ];
        let mut gate = gate("g", "true");
        gate.artifact = Some(ReleaseGateArtifactSelector {
            profile: None,
            path: Some("build/second.zip".to_string()),
        });
        let selected = select_gate_artifact(&artifacts, &gate).expect("artifact");
        assert_eq!(selected.path, "build/second.zip");
    }

    #[test]
    fn passing_gate_succeeds_with_substituted_artifact_and_records_evidence() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let dir = tempfile::tempdir().expect("component dir");
            std::fs::create_dir_all(dir.path().join("build")).expect("build dir");
            std::fs::write(dir.path().join("build/fixture.zip"), "artifact").expect("artifact");
            let component = component_at(dir.path());
            let state = state_with_artifact("build/fixture.zip");
            let mut gate = gate(
                "import",
                "test -f {artifact} && mkdir -p gate-evidence && cp {artifact} gate-evidence/checked.zip",
            );
            gate.evidence = Some("gate-evidence".to_string());

            let result =
                run_release_gate(&test_roots(), &component, &state, &gate).expect("gate run");

            assert_eq!(result.status, ReleaseStepStatus::Success);
            let data = result.data.expect("gate data");
            let evidence = data["evidence"].as_str().expect("evidence path");
            assert!(evidence.contains("/release/fixture/1_2_3/gates/import"));
            assert!(Path::new(evidence).join("checked.zip").is_file());
        });
    }

    #[test]
    fn artifact_substitution_is_shell_quoted() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let dir = tempfile::tempdir().expect("component dir");
            let spaced = dir.path().join("bu ild");
            std::fs::create_dir_all(&spaced).expect("build dir");
            std::fs::write(spaced.join("fix ture.zip"), "artifact").expect("artifact");
            let component = component_at(dir.path());
            let state = state_with_artifact("bu ild/fix ture.zip");

            let result = run_release_gate(
                &test_roots(),
                &component,
                &state,
                &gate("quote", "test -f {artifact}"),
            )
            .expect("gate run");

            assert_eq!(result.status, ReleaseStepStatus::Success);
        });
    }

    #[test]
    fn failing_gate_reports_gate_id_exit_code_and_output_tail() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let dir = tempfile::tempdir().expect("component dir");
            std::fs::create_dir_all(dir.path().join("build")).expect("build dir");
            std::fs::write(dir.path().join("build/fixture.zip"), "artifact").expect("artifact");
            let component = component_at(dir.path());
            let state = state_with_artifact("build/fixture.zip");

            let result = run_release_gate(
                &test_roots(),
                &component,
                &state,
                &gate("import", "echo import failed: bad site >&2; exit 7"),
            )
            .expect("gate run");

            assert_eq!(result.status, ReleaseStepStatus::Failed);
            let error = result.error.expect("gate error");
            assert!(error.contains("Release gate 'import' failed (exit 7)"));
            assert!(error.contains("import failed: bad site"));
        });
    }

    #[test]
    fn failing_gate_still_persists_declared_evidence() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let dir = tempfile::tempdir().expect("component dir");
            std::fs::create_dir_all(dir.path().join("build")).expect("build dir");
            std::fs::write(dir.path().join("build/fixture.zip"), "artifact").expect("artifact");
            let component = component_at(dir.path());
            let state = state_with_artifact("build/fixture.zip");
            let mut gate = gate(
                "import",
                "mkdir -p evidence; echo boom > evidence/log.txt; exit 1",
            );
            gate.evidence = Some("evidence".to_string());

            let result =
                run_release_gate(&test_roots(), &component, &state, &gate).expect("gate run");

            assert_eq!(result.status, ReleaseStepStatus::Failed);
            let data = result.data.expect("gate data");
            let evidence = data["evidence"].as_str().expect("evidence path");
            assert!(Path::new(evidence).join("log.txt").is_file());
        });
    }

    #[test]
    fn gate_with_no_artifacts_fails_with_guidance() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let dir = tempfile::tempdir().expect("component dir");
            let component = component_at(dir.path());
            let state = ReleaseState::default();

            let result = run_release_gate(&test_roots(), &component, &state, &gate("g", "true"))
                .expect("gate run");

            assert_eq!(result.status, ReleaseStepStatus::Failed);
            assert!(result
                .error
                .expect("gate error")
                .contains("no packaged artifacts"));
        });
    }

    #[test]
    fn output_tail_is_bounded_and_char_safe() {
        let long = "é".repeat(5000);
        let tail = output_tail(&long, 100);
        assert!(tail.len() <= 104);
        assert!(tail.starts_with('…'));
        assert_eq!(output_tail("short", 100), "short");
    }
}
