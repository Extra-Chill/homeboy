use std::io::Read;

use homeboy_core::stream_capture::StreamCaptureMetadata;

mod preflight;
mod prepare;
mod release_plan;
mod strategies;

pub(super) use prepare::{
    execute_preflighted_component_deploy, prepare_component_deploy, PreparedComponentDeploy,
};
pub(super) use release_plan::{
    release_artifact_plan, resolve_planned_release_artifact, ReleaseArtifactPlan,
};

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
    use super::prepare::prepare_component_deploy;
    use super::release_plan::{release_artifact_plan, should_try_download_release_artifact};
    use super::strategies::{cleanup_deploy_build_artifact, post_deploy_expected_version};
    use super::{bound_captured_read, ReleaseArtifactPlan};
    use crate::types::{DeployConfig, PreparedDeployArtifact};
    use homeboy_core::component::{
        ArtifactInput, Component, ComponentScriptsConfig, VersionTarget,
    };
    use homeboy_core::project::Project;
    use homeboy_core::test_support::run_git_command as git;
    use std::io::Write;

    #[test]
    fn bound_captured_read_keeps_trailing_tail_and_reports_truncation() {
        let (text, capture) = bound_captured_read(&b"Version: 1.2.3"[..], 64).expect("read");
        assert_eq!(text, "Version: 1.2.3");
        assert_eq!(capture.retained_bytes, 14);
        assert!(!capture.truncated);

        // The version string lives at the END of the manifest, so the retained
        // trailing tail must still surface it even when the head is dropped.
        let blob = format!("{}Version: 9.9.9", "x".repeat(100));
        let (text, capture) = bound_captured_read(blob.as_bytes(), 16).expect("read");
        assert_eq!(capture.seen_bytes, blob.len());
        assert_eq!(capture.retained_bytes, 16);
        assert!(capture.truncated);
        assert!(text.ends_with("Version: 9.9.9"));
    }

    #[test]
    fn prepared_artifact_skips_local_build_and_drives_post_deploy_version() {
        let temp = tempfile::tempdir().expect("tempdir");
        let durable_artifact = temp.path().join("release.tar.gz");
        let build_counter = temp.path().join("build-count");
        std::fs::write(&durable_artifact, "release payload").expect("artifact");
        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().display().to_string(),
            remote_path: "plugins/fixture".to_string(),
            build_artifact: Some("build/fixture.tar.gz".to_string()),
            extract_command: Some("tar -xzf {{artifact}}".to_string()),
            scripts: Some(ComponentScriptsConfig {
                build: vec![format!("printf build >> {}", build_counter.display())],
                ..ComponentScriptsConfig::default()
            }),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: vec![component.id.clone()],
            force: true,
            skip_build: true,
            expected_version: Some("1.2.3".to_string()),
            no_pull: true,
            prepared_artifact: Some(PreparedDeployArtifact {
                component_id: component.id.clone(),
                path: "build/fixture.tar.gz".to_string(),
                durable_path: durable_artifact.display().to_string(),
                size_bytes: 15,
                sha256: crate::sha256_file(&durable_artifact).expect("sha"),
                version: "1.2.3".to_string(),
                tag: "v1.2.3".to_string(),
                source_commit: "0123456789abcdef".to_string(),
            }),
            ..DeployConfig::default()
        };

        // The checkout reports a stale version; the prepared artifact's version
        // must drive post-deploy verification.
        let prepared = prepare_component_deploy(
            &component,
            &config,
            "/srv/site",
            &Project::default(),
            Some("0.8.1".to_string()),
            None,
            None,
        )
        .expect("prepared artifact should pass local preflight");

        assert_eq!(prepared.artifact_path, Some(durable_artifact.clone()));
        assert!(
            !build_counter.exists(),
            "prepared deployment must not build"
        );
        assert!(!prepared.cleanup_local_artifact);
        assert_eq!(prepared.local_version.as_deref(), Some("0.8.1"));
        assert_eq!(
            post_deploy_expected_version(&prepared).map(String::as_str),
            Some("1.2.3")
        );
        drop(prepared);
        assert!(
            durable_artifact.exists(),
            "caller-owned prepared artifacts remain outside downloaded-artifact cleanup"
        );
    }

    #[test]
    fn archive_artifact_requires_extract_command_unless_component_or_override_provides_one() {
        for (has_extract, has_override, required) in [
            (false, false, true),
            (false, true, false),
            (true, false, false),
        ] {
            assert_eq!(
                artifact_requires_component_extract_command(
                    std::path::Path::new("build/example.zip"),
                    has_extract,
                    has_override,
                ),
                required,
                "extract={has_extract} override={has_override}"
            );
        }
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
            skip_build: true,
            ..Default::default()
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
    fn non_release_refs_and_modes_skip_release_artifact_download() {
        let component = release_asset_component();
        let mut configs = vec![
            DeployConfig {
                head: true,
                ..Default::default()
            },
            DeployConfig {
                tagged: true,
                ..Default::default()
            },
        ];
        for requested_ref in [
            "main",
            "release/v0.55.1",
            "vnext",
            "9b780f558f0e4a1b2c3d4e5f60718293a4b5c6d7",
        ] {
            configs.push(deploy_config_with_ref(Some(requested_ref), false));
        }

        for config in configs {
            assert!(
                !should_try_download_release_artifact(&component, &config, false, false),
                "head={} tagged={} ref={:?} must build locally",
                config.head,
                config.tagged,
                config.requested_ref
            );
        }
    }

    #[test]
    fn package_dependencies_do_not_block_release_artifact_download() {
        for dependency in ["github:Extra-Chill/extrachill-tokens#v0.7.2", "^0.7.2"] {
            let temp = tempfile::tempdir().expect("tempdir");
            std::fs::write(
                temp.path().join("package.json"),
                format!(r#"{{"dependencies": {{"tokens": "{dependency}"}}}}"#),
            )
            .expect("write package.json");
            let component = Component {
                local_path: temp.path().to_string_lossy().to_string(),
                ..release_asset_component()
            };
            let config = DeployConfig {
                expected_version: Some("1.2.3".to_string()),
                ..Default::default()
            };

            assert!(
                should_try_download_release_artifact(&component, &config, false, false),
                "{dependency}"
            );
        }
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
            expected_version: Some("1.2.3".to_string()),
            ..Default::default()
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

    #[cfg(unix)]
    fn write_fake_binary(path: &std::path::Path, script: &str) {
        homeboy_test_support::write_executable(path, script, 0o755).expect("write fake binary");
    }

    /// #14782: a component resolved without a local checkout has no `git tag`
    /// to read. Without `--version`, the plan must fall back to the
    /// repository's latest GitHub Release instead of silently downgrading to
    /// a local build (which it cannot do anyway — there is no checkout).
    #[cfg(unix)]
    #[test]
    fn release_artifact_plan_without_expected_version_uses_latest_github_release_when_checkout_less(
    ) {
        let _lock = homeboy_core::test_support::env_lock();
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_fake_binary(
            &bin_dir.path().join("gh"),
            "#!/bin/sh\necho fake-test-token\nexit 0\n",
        );
        write_fake_binary(
            &bin_dir.path().join("curl"),
            "#!/bin/sh\ncat > /dev/null 2>&1\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n  \
             *releases/latest*) printf '%s' '{\"tag_name\":\"v0.176.17\"}'; printf '\\n200' ;;\n  \
             *) printf '\\n404' ;;\nesac\n",
        );
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = bin_dir.path().as_os_str().to_os_string();
        new_path.push(":");
        new_path.push(existing);
        let _path = homeboy_core::test_support::EnvVarGuard::set("PATH", new_path);

        let component = Component {
            id: "data-machine".to_string(),
            local_path: "/tmp/homeboy-14782-no-checkout".to_string(),
            remote_url: Some("https://github.com/Extra-Chill/data-machine".to_string()),
            build_artifact: Some("data-machine.zip".to_string()),
            ..Component::default()
        };
        let config = DeployConfig::default();

        match release_artifact_plan(&component, &config, false, false) {
            ReleaseArtifactPlan::Reuse { url, tag } => {
                assert_eq!(tag, "v0.176.17");
                assert_eq!(
                    url,
                    "https://github.com/Extra-Chill/data-machine/releases/download/v0.176.17/data-machine.zip"
                );
            }
            ReleaseArtifactPlan::LocalBuild { reason } => {
                panic!("expected release reuse plan from the latest GitHub release, got local build: {reason}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn explicit_checkout_less_version_agrees_with_release_artifact_plan() {
        let _lock = homeboy_core::test_support::env_lock();
        let bin = tempfile::tempdir().unwrap();
        write_fake_binary(&bin.path().join("gh"), "#!/bin/sh\necho fake-token\n");
        write_fake_binary(&bin.path().join("curl"),
            "#!/bin/sh\ncat > /dev/null 2>&1\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n *releases/tags/data-machine-events-v0.84.1) printf '{\"tag_name\":\"data-machine-events-v0.84.1\"}\\n200' ;;\n *) printf '\\n404' ;;\nesac\n");
        let mut path = bin.path().as_os_str().to_os_string();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let _path = homeboy_core::test_support::EnvVarGuard::set("PATH", path);
        for (id, expected) in [
            ("data-machine-events", "data-machine-events-v0.84.1"),
            ("root", "v0.84.1"),
        ] {
            let component = Component {
                id: id.into(),
                remote_url: Some("https://github.com/Extra-Chill/data-machine".into()),
                build_artifact: Some(format!("{id}.zip")),
                ..Component::default()
            };
            assert!(!component.has_local_checkout());
            let config = DeployConfig {
                expected_version: Some("0.84.1".into()),
                ..DeployConfig::default()
            };
            let deployed_ref =
                crate::orchestration_tag_checkout::deploy_tag_for_version(&component, "0.84.1")
                    .unwrap();
            assert_eq!(deployed_ref, expected);
            match release_artifact_plan(&component, &config, false, false) {
                ReleaseArtifactPlan::Reuse { tag, url } => {
                    assert_eq!(tag, deployed_ref);
                    assert_eq!(url, format!(
                        "https://github.com/Extra-Chill/data-machine/releases/download/{expected}/{id}.zip"
                    ));
                }
                ReleaseArtifactPlan::LocalBuild { reason } => panic!("{reason}"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn artifact_inputs_still_try_release_artifact_download() {
        let _lock = homeboy_core::test_support::env_lock();
        let _github = crate::test_support::OfflineGithub::not_found();
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
            expected_version: Some("1.2.3".to_string()),
            ..Default::default()
        };

        assert!(should_try_download_release_artifact(
            &component, &config, false, false
        ));
    }

    /// A transport failure while resolving a checkout-less component's release
    /// tag must not invent a tag: the plan falls back to a local build.
    #[cfg(unix)]
    #[test]
    fn failed_release_tag_lookup_plans_local_build() {
        let _lock = homeboy_core::test_support::env_lock();
        let _github = crate::test_support::OfflineGithub::unreachable();
        let component = Component {
            id: "example".to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            ..Component::default()
        };
        assert!(!component.has_local_checkout());
        let config = DeployConfig {
            expected_version: Some("1.2.3".to_string()),
            ..Default::default()
        };

        match release_artifact_plan(&component, &config, false, false) {
            ReleaseArtifactPlan::LocalBuild { reason } => {
                assert_eq!(reason, "no version tag found for release asset lookup");
            }
            ReleaseArtifactPlan::Reuse { tag, url } => {
                panic!("failed tag lookup must plan a local build, got reuse of {tag} at {url}");
            }
        }
    }

    #[test]
    fn cleanup_deploy_build_artifact_removes_only_component_owned_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let component = Component {
            id: "example".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        let build_dir = temp.path().join("build");
        std::fs::create_dir_all(&build_dir).expect("mkdir build");
        let artifact = build_dir.join("example.zip");
        let sibling = build_dir.join("keep.txt");
        std::fs::write(&artifact, b"zip").expect("write artifact");
        std::fs::write(&sibling, b"keep").expect("write sibling");
        cleanup_deploy_build_artifact(&component, &artifact);
        assert!(!artifact.exists());
        assert!(sibling.exists(), "non-empty build dir is preserved");

        std::fs::remove_file(&sibling).expect("remove sibling");
        std::fs::write(&artifact, b"zip").expect("write artifact");
        cleanup_deploy_build_artifact(&component, &artifact);
        assert!(!artifact.exists());
        assert!(!build_dir.exists(), "emptied build dir is removed");

        let outside_dir = tempfile::tempdir().expect("outside dir");
        let outside = outside_dir.path().join("example.zip");
        std::fs::write(&outside, b"zip").expect("write outside artifact");
        cleanup_deploy_build_artifact(&component, &outside);
        assert!(outside.exists(), "paths outside the component are ignored");
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
            head: true,
            ..Default::default()
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

    #[test]
    fn predeploy_artifact_version_inspection_compares_zip_version() {
        let temp = tempfile::tempdir().expect("tempdir");
        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            version_targets: Some(vec![VersionTarget {
                file: "fixture.php".to_string(),
                pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
                artifact_path: None,
            }]),
            ..Component::default()
        };
        let artifact = temp.path().join("fixture.zip");
        write_zip(
            &artifact,
            &[("fixture/fixture.php", "<?php\nVersion: 0.8.1\n")],
        );

        let error = validate_predeploy_artifact_version(&component, &artifact, "0.14.0")
            .expect_err("stale artifact version should fail preflight");
        assert!(
            error.contains("contains version '0.8.1'") && error.contains("expected '0.14.0'"),
            "unexpected error: {error}"
        );
        validate_predeploy_artifact_version(&component, &artifact, "0.8.1")
            .expect("matching artifact version should pass");
    }

    #[test]
    fn predeploy_artifact_version_inspection_checks_root_and_nested_plugin_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("fixture.zip");
        write_zip(
            &artifact,
            &[
                ("fixture/package.json", r#"{"version":"1.2.3"}"#),
                ("fixture/plugin/plugin.php", "<?php\n/* Version: 1.2.3 */\n"),
            ],
        );
        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            version_targets: Some(vec![
                VersionTarget {
                    file: "package.json".to_string(),
                    pattern: Some(r#""version"\s*:\s*"([0-9.]+)""#.to_string()),
                    artifact_path: None,
                },
                VersionTarget {
                    file: "plugin/plugin.php".to_string(),
                    pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
                    artifact_path: None,
                },
            ]),
            ..Component::default()
        };

        validate_predeploy_artifact_version(&component, &artifact, "1.2.3")
            .expect("every declared artifact target matches the resolved version");
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
    fn predeploy_artifact_version_inspection_ignores_repository_only_targets_when_archive_paths_are_declared(
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("wp-codebox.zip");
        write_zip(
            &artifact,
            &[
                (
                    "wp-codebox/wp-codebox.php",
                    "<?php\n/* Version: 0.22.0 */\n",
                ),
                (
                    "wp-codebox/packages/cli/package.json",
                    r#"{"version":"0.22.0"}"#,
                ),
            ],
        );
        let component = Component {
            id: "wp-codebox".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            version_targets: Some(vec![
                VersionTarget {
                    file: "npm-shrinkwrap.json".to_string(),
                    pattern: Some(r#""version"\s*:\s*"([0-9.]+)""#.to_string()),
                    artifact_path: None,
                },
                VersionTarget {
                    file: "wp-codebox.php".to_string(),
                    pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
                    artifact_path: Some("wp-codebox/wp-codebox.php".to_string()),
                },
                VersionTarget {
                    file: "packages/cli/package.json".to_string(),
                    pattern: Some(r#""version"\s*:\s*"([0-9.]+)""#.to_string()),
                    artifact_path: Some("wp-codebox/packages/cli/package.json".to_string()),
                },
            ]),
            ..Component::default()
        };

        validate_predeploy_artifact_version(&component, &artifact, "0.22.0")
            .expect("archive targets, not the repository lockfile, verify the release asset");
    }

    #[test]
    fn predeploy_artifact_version_inspection_rejects_contradictory_archive_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("fixture.zip");
        write_zip(
            &artifact,
            &[
                ("plugin/plugin.php", "Version: 1.2.3\n"),
                ("plugin/package.json", r#"{"version":"1.2.4"}"#),
            ],
        );
        let component = Component {
            id: "fixture".to_string(),
            version_targets: Some(vec![
                VersionTarget {
                    file: "plugin.php".to_string(),
                    pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
                    artifact_path: Some("plugin/plugin.php".to_string()),
                },
                VersionTarget {
                    file: "package.json".to_string(),
                    pattern: Some(r#""version"\s*:\s*"([0-9.]+)""#.to_string()),
                    artifact_path: Some("plugin/package.json".to_string()),
                },
            ]),
            ..Component::default()
        };

        let error = validate_predeploy_artifact_version(&component, &artifact, "1.2.3")
            .expect_err("contradictory archive target must fail closed");
        assert!(error.contains("1.2.4") && error.contains("expected '1.2.3'"));
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

    /// A component that is eligible for release-asset reuse, so these tests
    /// isolate the ref/tag decision rather than the eligibility preconditions.
    fn release_asset_component() -> Component {
        Component {
            id: "example".to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("build/example.zip".to_string()),
            ..Component::default()
        }
    }

    fn deploy_config_with_ref(requested_ref: Option<&str>, tagged: bool) -> DeployConfig {
        DeployConfig {
            requested_ref: requested_ref.map(str::to_string),
            tagged,
            ..DeployConfig::default()
        }
    }

    /// The regression: an exact tag ref used to force `local_build`, which ships
    /// locally rebuilt bytes instead of the canonical release package and then
    /// reads back as `remote_modified` against it (#12215).
    #[test]
    fn tag_valued_ref_reuses_the_canonical_release_asset() {
        let component = release_asset_component();
        let config = deploy_config_with_ref(Some("v0.55.1"), false);

        match release_artifact_plan(&component, &config, false, false) {
            ReleaseArtifactPlan::Reuse { tag, url } => {
                // The asset is bound to the requested ref, not to whatever the
                // latest local tag happens to be.
                assert_eq!(tag, "v0.55.1");
                assert!(url.contains("v0.55.1"), "{url}");
                assert!(url.ends_with("example.zip"), "{url}");
            }
            ReleaseArtifactPlan::LocalBuild { reason } => {
                panic!("tag-valued --ref must reuse the release asset, got local_build: {reason}")
            }
        }
    }

    /// `--tagged` is the escape hatch for "I want a local build of this tag",
    /// including when the tag has a reusable asset, so it must outrank `--ref`.
    #[test]
    fn tagged_outranks_a_tag_valued_ref() {
        let component = release_asset_component();
        let config = deploy_config_with_ref(Some("v0.55.1"), true);

        match release_artifact_plan(&component, &config, false, false) {
            ReleaseArtifactPlan::LocalBuild { reason } => {
                assert!(reason.contains("--tagged"), "{reason}")
            }
            ReleaseArtifactPlan::Reuse { tag, .. } => {
                panic!("--tagged must force a local build, got release asset reuse for {tag}")
            }
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
