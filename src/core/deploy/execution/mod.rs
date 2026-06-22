use std::io::Read;

use crate::core::stream_capture::StreamCaptureMetadata;

mod preflight;
mod prepare;
mod release_plan;
mod strategies;

pub(super) use prepare::{
    execute_preflighted_component_deploy, prepare_component_deploy, PreparedComponentDeploy,
};
pub(super) use release_plan::{release_artifact_plan, ReleaseArtifactPlan};

/// Maximum number of bytes retained when reading a version-target file out of a
/// deploy artifact. The artifact is downloaded release content and therefore
/// attacker-influenced, so the retained bytes are capped with truncation
/// metadata rather than slurping an unbounded `read_to_string`. Mirrors the
/// bounded-capture pattern used by `agent_task_promotion` / runner exec captures
/// (#5363). The cap is generous: a real version manifest is a few kilobytes, so
/// the trailing window still contains any plausible version string.
const ARTIFACT_VERSION_READ_LIMIT_BYTES: usize = 65_536;

/// Read at most `limit` bytes from `reader`, keeping the trailing tail (the most
/// relevant window for a version string that lives near the end of a manifest)
/// and returning the retained text plus truncation metadata. Mirrors the
/// `bound_captured_stream` pattern in `agent_task_promotion` so artifact reads
/// cannot grow without bound.
fn bound_captured_read<R: Read>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<(String, StreamCaptureMetadata)> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    let seen = bytes.len();
    let retained: &[u8] = if seen > limit {
        &bytes[seen - limit..]
    } else {
        &bytes
    };
    let metadata = StreamCaptureMetadata {
        limit_bytes: limit,
        seen_bytes: seen,
        retained_bytes: retained.len(),
        truncated: seen > retained.len(),
    };
    Ok((String::from_utf8_lossy(retained).to_string(), metadata))
}

#[cfg(test)]
mod tests {
    use super::preflight::{
        artifact_requires_component_extract_command, resolve_preflight_artifact_path,
        validate_predeploy_artifact_version,
    };
    use super::prepare::failed_component_deploy_result;
    use super::release_plan::{release_artifact_plan, should_try_download_release_artifact};
    use super::strategies::cleanup_deploy_build_artifact;
    use super::{bound_captured_read, ReleaseArtifactPlan, ARTIFACT_VERSION_READ_LIMIT_BYTES};
    use crate::core::component::{ArtifactInput, Component, VersionTarget};
    use crate::core::deploy::types::DeployConfig;
    use std::io::Write;
    use std::process::Command;

    #[test]
    fn bound_captured_read_retains_full_source_within_limit() {
        let (text, capture) = bound_captured_read(&b"Version: 1.2.3"[..], 64).expect("read");
        assert_eq!(text, "Version: 1.2.3");
        assert_eq!(capture.seen_bytes, 14);
        assert_eq!(capture.retained_bytes, 14);
        assert_eq!(capture.limit_bytes, 64);
        assert!(!capture.truncated);
    }

    #[test]
    fn bound_captured_read_keeps_trailing_tail_when_truncated() {
        // The version string lives at the END of the manifest, so the retained
        // trailing tail must still surface it even when the head is dropped.
        let blob = format!("{}Version: 9.9.9", "x".repeat(100));
        let (text, capture) = bound_captured_read(blob.as_bytes(), 16).expect("read");
        assert_eq!(capture.limit_bytes, 16);
        assert_eq!(capture.seen_bytes, blob.len());
        assert_eq!(capture.retained_bytes, 16);
        assert!(capture.truncated);
        assert!(text.ends_with("Version: 9.9.9"));
    }

    #[test]
    fn bound_captured_read_default_cap_is_nonzero() {
        assert!(ARTIFACT_VERSION_READ_LIMIT_BYTES > 0);
    }

    #[test]
    fn test_execute_component_deploy_failure_helper_preserves_build_exit_code() {
        let component = Component {
            id: "example".to_string(),
            ..Component::default()
        };

        let result = failed_component_deploy_result(
            &component,
            "/srv/site",
            Some("1.0.0".to_string()),
            Some("0.9.0".to_string()),
            Some(7),
            "deploy failed".to_string(),
        );

        assert_eq!(result.id, "example");
        assert_eq!(result.status, "failed");
        assert_eq!(result.local_version.as_deref(), Some("1.0.0"));
        assert_eq!(result.remote_version.as_deref(), Some("0.9.0"));
        assert_eq!(result.build_exit_code, Some(7));
        assert_eq!(result.error.as_deref(), Some("deploy failed"));
    }

    #[test]
    fn archive_artifact_without_component_extract_is_allowed_by_deploy_override() {
        assert!(!artifact_requires_component_extract_command(
            std::path::Path::new("build/example.zip"),
            false,
            true,
        ));
    }

    #[test]
    fn archive_artifact_without_component_extract_or_override_requires_extract_command() {
        assert!(artifact_requires_component_extract_command(
            std::path::Path::new("build/example.zip"),
            false,
            false,
        ));
    }

    #[test]
    fn archive_artifact_preflight_hint_uses_double_brace_placeholder() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("build/example.zip");
        std::fs::create_dir_all(artifact.parent().expect("artifact parent")).expect("build dir");
        std::fs::write(&artifact, "zip bytes").expect("artifact");
        let component = Component {
            id: "example".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            build_artifact: Some("build/example.zip".to_string()),
            extract_command: None,
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: true,
            keep_deps: false,
            expected_version: None,
            no_pull: false,
            head: false,
            tagged: false,
        };

        let result = resolve_preflight_artifact_path(
            &component,
            &config,
            "/srv/site",
            temp.path().to_str().expect("install dir"),
            None,
            None,
            None,
            None,
        )
        .expect_err("archive without extract command should fail preflight");
        let error = result.error.expect("preflight error");

        assert!(
            error.contains("unzip -o {{artifact}} && rm {{artifact}}"),
            "hint must contain double-brace placeholder: {error}"
        );
        assert!(
            !error.contains("unzip -o {artifact} && rm {artifact}"),
            "hint must not suggest the single-brace placeholder form: {error}"
        );
    }

    #[test]
    fn archive_artifact_with_component_extract_does_not_require_another_command() {
        assert!(!artifact_requires_component_extract_command(
            std::path::Path::new("build/example.zip"),
            true,
            false,
        ));
    }

    #[test]
    fn head_deploy_skips_release_artifact_download() {
        let component = Component {
            id: "example".to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: None,
            no_pull: false,
            head: true,
            tagged: false,
        };

        assert!(!should_try_download_release_artifact(
            &component, &config, false, false
        ));
    }

    #[test]
    fn tagged_deploy_skips_release_artifact_download() {
        let component = Component {
            id: "example".to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: None,
            no_pull: false,
            head: false,
            tagged: true,
        };

        assert!(!should_try_download_release_artifact(
            &component, &config, false, false
        ));
    }

    #[test]
    fn mutable_package_dependencies_still_try_release_artifact_download() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("package.json"),
            r#"{
                "dependencies": {
                    "tokens": "github:Extra-Chill/extrachill-tokens#v0.7.2"
                }
            }"#,
        )
        .expect("write package.json");
        let component = Component {
            id: "example".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: Some("1.2.3".to_string()),
            no_pull: false,
            head: false,
            tagged: false,
        };

        assert!(should_try_download_release_artifact(
            &component, &config, false, false
        ));
    }

    #[test]
    fn registry_package_dependencies_allow_release_artifact_download_for_expected_version() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("package.json"),
            r#"{
                "dependencies": {
                    "tokens": "^0.7.2"
                }
            }"#,
        )
        .expect("write package.json");
        let component = Component {
            id: "example".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: Some("1.2.3".to_string()),
            no_pull: false,
            head: false,
            tagged: false,
        };

        assert!(should_try_download_release_artifact(
            &component, &config, false, false
        ));
    }

    #[test]
    fn release_artifact_plan_uses_expected_version_tag_url() {
        let temp = tempfile::tempdir().expect("tempdir");
        let component = Component {
            id: "example".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: Some("1.2.3".to_string()),
            no_pull: false,
            head: false,
            tagged: false,
        };

        match release_artifact_plan(&component, &config, false, false) {
            ReleaseArtifactPlan::Reuse { url, tag } => {
                assert_eq!(tag, "v1.2.3");
                assert_eq!(
                    url,
                    "https://github.com/example/example/releases/download/v1.2.3/example.zip"
                );
            }
            ReleaseArtifactPlan::LocalBuild { reason } => {
                panic!("expected release reuse plan, got local build: {reason}");
            }
        }
    }

    #[test]
    fn artifact_inputs_still_try_release_artifact_download() {
        let component = Component {
            id: "example".to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            artifact_inputs: vec![ArtifactInput {
                component: "producer".to_string(),
                artifact: "build/producer.zip".to_string(),
                target: "runtime/packages/producer.zip".to_string(),
                sha256: None,
            }],
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: Some("1.2.3".to_string()),
            no_pull: false,
            head: false,
            tagged: false,
        };

        assert!(should_try_download_release_artifact(
            &component, &config, false, false
        ));
    }

    #[test]
    fn cleanup_deploy_build_artifact_removes_zip_and_empty_build_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let build_dir = temp.path().join("build");
        std::fs::create_dir_all(&build_dir).expect("mkdir build");
        let artifact = build_dir.join("example.zip");
        std::fs::write(&artifact, b"zip").expect("write artifact");
        let component = Component {
            id: "example".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        cleanup_deploy_build_artifact(&component, &artifact);

        assert!(!artifact.exists());
        assert!(!build_dir.exists());
    }

    #[test]
    fn preflight_creates_missing_archive_artifact_from_tracked_head() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("plugin.php"), "<?php\n").expect("write plugin");
        std::fs::create_dir_all(temp.path().join("node_modules")).expect("mkdir node_modules");
        std::fs::write(temp.path().join("node_modules/junk.js"), "junk\n")
            .expect("write untracked dependency");
        git(temp.path(), &["init"]);
        git(temp.path(), &["add", "plugin.php"]);
        git(
            temp.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );

        let component = Component {
            id: "demo-plugin".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            build_artifact: Some("build/demo-plugin.zip".to_string()),
            extract_command: Some("unzip -o {{artifact}} && rm {{artifact}}".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: Vec::new(),
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            expected_version: None,
            no_pull: false,
            head: true,
            tagged: false,
        };

        let artifact = resolve_preflight_artifact_path(
            &component,
            &config,
            "/srv/site",
            "/srv/site/wp-content/plugins/demo-plugin",
            None,
            None,
            Some(0),
            None,
        )
        .expect("archive artifact should resolve");

        assert_eq!(artifact, temp.path().join("build/demo-plugin.zip"));
        let file = std::fs::File::open(&artifact).expect("open zip");
        let mut zip = zip::ZipArchive::new(file).expect("read zip");
        assert!(zip.by_name("demo-plugin/plugin.php").is_ok());
        assert!(zip.by_name("demo-plugin/node_modules/junk.js").is_err());
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn cleanup_deploy_build_artifact_preserves_non_empty_build_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let build_dir = temp.path().join("build");
        std::fs::create_dir_all(&build_dir).expect("mkdir build");
        let artifact = build_dir.join("example.zip");
        let sibling = build_dir.join("keep.txt");
        std::fs::write(&artifact, b"zip").expect("write artifact");
        std::fs::write(&sibling, b"keep").expect("write sibling");
        let component = Component {
            id: "example".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        cleanup_deploy_build_artifact(&component, &artifact);

        assert!(!artifact.exists());
        assert!(build_dir.exists());
        assert!(sibling.exists());
    }

    #[test]
    fn cleanup_deploy_build_artifact_ignores_paths_outside_component() {
        let component_dir = tempfile::tempdir().expect("component dir");
        let outside_dir = tempfile::tempdir().expect("outside dir");
        let artifact = outside_dir.path().join("example.zip");
        std::fs::write(&artifact, b"zip").expect("write artifact");
        let component = Component {
            id: "example".to_string(),
            local_path: component_dir.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        cleanup_deploy_build_artifact(&component, &artifact);

        assert!(artifact.exists());
    }

    #[test]
    fn predeploy_artifact_version_inspection_rejects_mismatched_zip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("fixture.zip");
        write_zip(
            &artifact,
            &[("fixture/fixture.php", "<?php\nVersion: 0.8.1\n")],
        );
        let component = versioned_zip_component(temp.path());

        let error = validate_predeploy_artifact_version(&component, &artifact, "0.14.0")
            .expect_err("stale artifact version should fail preflight");

        assert!(
            error.contains("contains version '0.8.1'") && error.contains("expected '0.14.0'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn predeploy_artifact_version_inspection_accepts_matching_zip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("fixture.zip");
        write_zip(
            &artifact,
            &[("fixture/fixture.php", "<?php\nVersion: 0.14.0\n")],
        );
        let component = versioned_zip_component(temp.path());

        validate_predeploy_artifact_version(&component, &artifact, "0.14.0")
            .expect("matching artifact version should pass");
    }

    #[test]
    fn predeploy_artifact_version_inspection_uses_artifact_path_override() {
        // Mirrors @wordpress/scripts plugins: bump source `blocks/<block>/block.json`
        // but ship the compiled `build/<block>/block.json` (source `blocks/` excluded
        // from the ZIP). The verifier must check the artifact_path inside the ZIP.
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("fixture.zip");
        write_zip(
            &artifact,
            &[(
                "fixture/build/login-register/block.json",
                "{\n  \"version\": \"0.17.2\"\n}\n",
            )],
        );
        let component = block_json_component(temp.path());

        validate_predeploy_artifact_version(&component, &artifact, "0.17.2")
            .expect("artifact_path override should verify against the compiled build/ path");
    }

    #[test]
    fn predeploy_artifact_version_inspection_reports_artifact_path_when_missing() {
        // When artifact_path is set but absent from the ZIP, the error should name the
        // artifact path (what ships), not the source bump path.
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("fixture.zip");
        write_zip(&artifact, &[("fixture/fixture.php", "<?php\n")]);
        let component = block_json_component(temp.path());

        let error = validate_predeploy_artifact_version(&component, &artifact, "0.17.2")
            .expect_err("missing artifact_path entry should fail preflight");

        assert!(
            error.contains("build/login-register/block.json"),
            "error should reference the artifact_path, got: {error}"
        );
        assert!(
            !error.contains("blocks/login-register/block.json"),
            "error should not reference the source bump path, got: {error}"
        );
    }

    fn versioned_zip_component(local_path: &std::path::Path) -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: local_path.to_string_lossy().to_string(),
            version_targets: Some(vec![VersionTarget {
                file: "fixture.php".to_string(),
                pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
                artifact_path: None,
            }]),
            ..Component::default()
        }
    }

    fn block_json_component(local_path: &std::path::Path) -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: local_path.to_string_lossy().to_string(),
            version_targets: Some(vec![VersionTarget {
                // Bumped in the workspace (git-tracked source).
                file: "blocks/login-register/block.json".to_string(),
                pattern: Some(r#""version":\s*"([0-9.]+)""#.to_string()),
                // Verified inside the shipped artifact (compiled build/ output).
                artifact_path: Some("build/login-register/block.json".to_string()),
            }]),
            ..Component::default()
        }
    }

    fn write_zip(path: &std::path::Path, files: &[(&str, &str)]) {
        let file = std::fs::File::create(path).expect("zip file");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();

        for (name, contents) in files {
            zip.start_file(*name, options).expect("zip entry");
            zip.write_all(contents.as_bytes()).expect("zip contents");
        }

        zip.finish().expect("finish zip");
    }
}
