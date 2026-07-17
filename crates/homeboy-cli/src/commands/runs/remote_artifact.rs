use std::fs;
use std::path::{Path, PathBuf};

use homeboy::core::observation::artifact_preview;
use homeboy::core::observation::runs_service::{
    self, PersistedArtifactCleanupOptions, RunnerDownloadCleanupOptions,
};
use homeboy::core::observation::ArtifactRecord;
use homeboy::core::observation::ObservationStore;
use homeboy::core::resource_cleanup_intent::ResourceCleanupIntent;
use homeboy::core::resource_lifecycle_index::{
    ResourceCleanupPolicy, ResourceEvidenceRetention, ResourceLifecycleRecord,
    ResourceLifecycleResourceStatus,
};
use homeboy::core::{agent_tasks::lifecycle as agent_task_lifecycle, server, Error};
use homeboy::runner::artifact_attach::{
    self as runner_artifact_attach, RunnerAttachArtifactType, RunnerAttachSource,
};
use homeboy::runner::runners::{self as runner, Runner, RunnerKind};

use super::types::{
    RunsArtifactAttachArgs, RunsArtifactAttachOutput, RunsArtifactCaptureArgs,
    RunsArtifactCaptureBrowser, RunsArtifactCaptureOutput, RunsArtifactCapturePage,
    RunsArtifactCaptureViewport, RunsArtifactCleanupDownloadsArgs,
    RunsArtifactCleanupDownloadsOutput, RunsArtifactCleanupPersistedArgs,
    RunsArtifactCleanupPersistedOutput, RunsArtifactGetOutput, RunsArtifactPreviewArgs,
    RunsArtifactPreviewOutput, RunsOutput,
};
use super::CmdResult;
use crate::commands::cleanup::RUNNER_DOWNLOADS_METADATA;

pub fn get(artifact: ArtifactRecord, output: Option<PathBuf>) -> CmdResult<RunsOutput> {
    let download = runs_service::download_remote_artifact(artifact, output)?;
    Ok((
        RunsOutput::ArtifactGet(RunsArtifactGetOutput {
            command: "runs.artifact.get",
            run_id: download.run_id,
            artifact_id: download.artifact_id,
            runner_id: download
                .artifact_ref
                .as_ref()
                .map(|artifact_ref| {
                    artifact_ref
                        .path
                        .as_deref()
                        .and_then(runner_id_from_artifact_ref)
                        .unwrap_or_default()
                        .to_string()
                })
                .filter(|runner_id| !runner_id.is_empty()),
            source_content_url: None,
            output_path: download.output_path.display().to_string(),
            content_type: download.content_type,
            size_bytes: download.size_bytes,
            sha256: download.sha256,
            artifact_ref: download.artifact_ref,
        }),
        0,
    ))
}

fn runner_id_from_artifact_ref(path: &str) -> Option<&str> {
    let path = path.strip_prefix("runner-artifact://")?;
    path.split('/').next()
}

pub fn attach(args: RunsArtifactAttachArgs) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    runs_service::require_run(&store, &args.run_id)?;
    validate_artifact_name(&args.name)?;
    let runner = runner::load(&args.runner)?;
    validate_runner_artifact_path(&runner, &args.path)?;

    let source = runner_artifact_attach::copy_runner_artifact_source(&runner, &args.path)?;
    let metadata = runner_attach_metadata(&runner, &args.run_id, &args.path, &source);
    let artifact = match source.artifact_type {
        RunnerAttachArtifactType::File => {
            store.record_artifact_with_metadata(&args.run_id, &args.name, &source.path, metadata)?
        }
        RunnerAttachArtifactType::Directory => store.record_directory_artifact_with_metadata(
            &args.run_id,
            &args.name,
            &source.path,
            metadata,
        )?,
    };
    let artifact = store.update_artifact_metadata(
        &artifact.id,
        metadata_with_resource_lifecycle(artifact.metadata_json.clone(), &artifact, &args.runner),
    )?;
    runner_artifact_attach::cleanup_runner_attach_source(&source);

    Ok((
        RunsOutput::ArtifactAttach(RunsArtifactAttachOutput {
            command: "runs.artifact.attach",
            run_id: args.run_id,
            runner_id: args.runner,
            source_path: args.path,
            artifact,
        }),
        0,
    ))
}

fn metadata_with_resource_lifecycle(
    mut metadata: serde_json::Value,
    artifact: &ArtifactRecord,
    runner_id: &str,
) -> serde_json::Value {
    if !metadata.is_object() {
        metadata = serde_json::json!({});
    }
    let resource = ResourceLifecycleRecord {
        owner: "homeboy-runs".to_string(),
        run_id: artifact.run_id.clone(),
        runner_id: Some(runner_id.to_string()),
        path: artifact.path.clone(),
        root_bound: None,
        kind: format!("artifact_{}", artifact.artifact_type),
        ttl: Some("P30D".to_string()),
        cleanup_policy: ResourceCleanupPolicy::DeleteAfterTtl,
        evidence_retention: ResourceEvidenceRetention::Full,
        cleanup_intent: ResourceCleanupIntent::DryRun,
        cleanup_command: Some(format!(
            "homeboy runs resources --run-id {} --cleanup-plan",
            artifact.run_id
        )),
        status: ResourceLifecycleResourceStatus::Retained,
    };
    metadata["resource_lifecycle"] =
        serde_json::to_value(resource).expect("resource lifecycle records are serializable");
    metadata
}

pub fn preview(args: RunsArtifactPreviewArgs) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let artifact = runs_service::resolve_artifact_for_run(&store, &args.run_id, &args.artifact_id)?;
    if artifact.artifact_type != "directory" {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} is {}, not a previewable directory",
                artifact.id, artifact.artifact_type
            ),
            Some(artifact.id),
            Some(vec![
                "Run `homeboy runs artifacts <run-id>` to find directory artifacts.".to_string(),
            ]),
        ));
    }

    let artifact_path = PathBuf::from(&artifact.path);
    if !artifact_path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} directory is missing or unreadable at {}",
                artifact.id,
                artifact_path.display()
            ),
            Some(artifact.id),
            None,
        ));
    }

    let port = args
        .port
        .map(Ok)
        .unwrap_or_else(artifact_preview::available_port)?;
    let base_url = format!("http://127.0.0.1:{port}/");
    let child = artifact_preview::start_static_artifact_server(&artifact_path, port, "preview")?;
    let process_id = child.id();
    drop(child);

    let entrypoints = homeboy::core::artifacts::html_preview_entrypoints(&artifact)
        .into_iter()
        .map(|mut entrypoint| {
            entrypoint.public_url = reqwest::Url::parse(&base_url)
                .ok()
                .and_then(|url| url.join(&entrypoint.path).ok())
                .map(|url| url.to_string());
            entrypoint
        })
        .collect();

    Ok((
        RunsOutput::ArtifactPreview(RunsArtifactPreviewOutput {
            command: "runs.artifact.preview",
            run_id: args.run_id,
            artifact_id: artifact.id,
            artifact_path: artifact_path.display().to_string(),
            base_url,
            process_id,
            entrypoints,
            stop_hint: format!("Stop preview server with `kill {process_id}`."),
        }),
        0,
    ))
}

pub fn capture(args: RunsArtifactCaptureArgs) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let artifact = runs_service::resolve_artifact_for_run(&store, &args.run_id, &args.artifact_id)?;
    if artifact.artifact_type != "directory" {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} is {}, not a capturable directory",
                artifact.id, artifact.artifact_type
            ),
            Some(artifact.id),
            Some(vec![
                "Run `homeboy runs artifacts <run-id>` to find directory artifacts.".to_string(),
            ]),
        ));
    }

    let artifact_path = PathBuf::from(&artifact.path);
    if !artifact_path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} directory is missing or unreadable at {}",
                artifact.id,
                artifact_path.display()
            ),
            Some(artifact.id),
            None,
        ));
    }

    let viewport = RunsArtifactCaptureViewport {
        width: args.viewport_width,
        height: args.viewport_height,
    };
    let (base_url, provider, captured) = artifact_preview::capture_artifact_entrypoints(
        &artifact_path,
        &args.output_dir,
        &args.entrypoints,
        artifact_preview::CaptureViewport {
            width: args.viewport_width,
            height: args.viewport_height,
        },
        args.port,
    )?;

    let pages = captured
        .into_iter()
        .map(|page| RunsArtifactCapturePage {
            entrypoint: page.entrypoint,
            page_url: page.page_url,
            screenshot_path: page.screenshot_path.display().to_string(),
            viewport: viewport.clone(),
            status: page.status,
            timing_ms: page.timing_ms,
            error: page.error,
        })
        .collect::<Vec<_>>();

    let manifest_path = args.output_dir.join("capture-manifest.json");
    let browser = RunsArtifactCaptureBrowser {
        command: provider.command,
        available: provider.available,
        error: provider.error,
    };
    let output = RunsArtifactCaptureOutput {
        command: "runs.artifact.capture",
        run_id: args.run_id,
        artifact_id: artifact.id,
        artifact_path: artifact_path.display().to_string(),
        output_dir: args.output_dir.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        base_url,
        viewport,
        browser,
        pages,
    };
    artifact_preview::write_capture_manifest(&manifest_path, &output)?;
    let exit_code = if output.pages.iter().all(|page| page.status == "captured") {
        0
    } else {
        1
    };

    Ok((RunsOutput::ArtifactCapture(output), exit_code))
}

fn runner_attach_metadata(
    runner: &Runner,
    run_id: &str,
    runner_path: &str,
    source: &RunnerAttachSource,
) -> serde_json::Value {
    let mut metadata = serde_json::json!({
            "source": "runner_path_attach",
            "runner_id": runner.id,
            "runner_path": runner_path,
            "source_type": match source.artifact_type {
                RunnerAttachArtifactType::File => "file",
                RunnerAttachArtifactType::Directory => "directory",
            },
    });
    if source.artifact_type == RunnerAttachArtifactType::Directory
        && directory_contains_html(&source.path)
    {
        metadata["role"] = serde_json::Value::String("static_site_artifact".to_string());
    }
    if let Ok(record) = agent_task_lifecycle::status(run_id) {
        if let Some(binding) = retained_agent_task_binding(&record, &runner.id) {
            metadata["agent_task"] = binding;
        }
    }
    metadata
}

fn retained_agent_task_binding(
    record: &homeboy::core::agent_tasks::lifecycle::AgentTaskRunRecord,
    runner_id: &str,
) -> Option<serde_json::Value> {
    let runner_job_id = record.runner_job_id()?;
    (record.runner_id() == Some(runner_id)).then(|| {
        serde_json::json!({
            "retained_runner_binding": {
                "runner_id": runner_id,
                "runner_job_id": runner_job_id,
            }
        })
    })
}

fn validate_artifact_name(name: &str) -> homeboy::core::Result<()> {
    if name.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "name",
            "artifact name must not be empty",
            Some(name.to_string()),
            None,
        ));
    }
    Ok(())
}

fn validate_runner_artifact_path(runner: &Runner, path: &str) -> homeboy::core::Result<()> {
    let roots = allowed_runner_artifact_roots(runner)?;
    match homeboy::core::paths::authorize_remote_artifact_path(
        Path::new(path),
        &roots,
        homeboy::core::paths::RemotePathRootContainment::RemoteString,
    ) {
        Ok(()) => Ok(()),
        Err(homeboy::core::paths::RemotePathAuthorizationError::NotAbsolute) => {
            Err(Error::validation_invalid_argument(
                "path",
                "runner artifact attach requires an absolute runner-side path",
                Some(path.to_string()),
                None,
            ))
        }
        Err(homeboy::core::paths::RemotePathAuthorizationError::ContainsParentDir) => {
            Err(Error::validation_invalid_argument(
                "path",
                "runner artifact attach path must not contain parent directory components",
                Some(path.to_string()),
                None,
            ))
        }
        Err(homeboy::core::paths::RemotePathAuthorizationError::OutsideAllowedRoots) => {
            Err(Error::validation_invalid_argument(
                "path",
                "runner artifact path must be under an allowed runner workspace/output root",
                Some(path.to_string()),
                Some(vec![format!("Allowed roots: {}", roots.join(", "))]),
            ))
        }
    }
}

fn allowed_runner_artifact_roots(runner: &Runner) -> homeboy::core::Result<Vec<String>> {
    let mut roots = Vec::new();
    if let Some(root) = runner.workspace_root.as_deref() {
        roots.push(homeboy::core::paths::normalize_remote_root(root));
    }
    roots.extend(
        runner
            .policy
            .workspace_roots
            .iter()
            .map(|root| homeboy::core::paths::normalize_remote_root(root)),
    );
    if let Some(root) = runner.env.get("HOMEBOY_ARTIFACT_ROOT") {
        roots.push(homeboy::core::paths::normalize_remote_root(root));
    }
    if runner.kind == RunnerKind::Local {
        roots.push(homeboy::core::paths::normalize_remote_root(
            &homeboy::core::artifact_root()?.display().to_string(),
        ));
    }
    roots.sort();
    roots.dedup();
    roots.retain(|root| Path::new(root).is_absolute());
    if roots.is_empty() {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner has no configured workspace/output roots for artifact attach",
            Some(runner.id.clone()),
            Some(vec![
                "Set the runner workspace_root, policy.workspace_roots, or HOMEBOY_ARTIFACT_ROOT."
                    .to_string(),
            ]),
        ));
    }
    Ok(roots)
}

fn directory_contains_html(path: &Path) -> bool {
    directory_contains_html_inner(path, 0)
}

fn directory_contains_html_inner(path: &Path, seen: usize) -> bool {
    if seen >= 100 {
        return false;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    for (index, entry) in entries.flatten().enumerate() {
        if seen + index >= 100 {
            return false;
        }
        let path = entry.path();
        if path.is_dir() && directory_contains_html_inner(&path, seen + index + 1) {
            return true;
        }
        let lower = path.to_string_lossy().to_ascii_lowercase();
        if lower.ends_with(".html") || lower.ends_with(".htm") {
            return true;
        }
    }
    false
}

pub fn cleanup_downloads(args: RunsArtifactCleanupDownloadsArgs) -> CmdResult<RunsOutput> {
    let outcome = runs_service::cleanup_runner_downloads(RunnerDownloadCleanupOptions {
        apply: args.apply,
        runner: args.runner,
        run_id: args.run_id,
    })?;

    Ok((
        RunsOutput::ArtifactCleanupDownloads(RunsArtifactCleanupDownloadsOutput {
            command: "runs.artifact.cleanup-downloads",
            cleanup_category: RUNNER_DOWNLOADS_METADATA.category,
            canonical_cleanup_command: RUNNER_DOWNLOADS_METADATA
                .canonical_cleanup_command(args.apply),
            specialist_cleanup_command: RUNNER_DOWNLOADS_METADATA.specialist_command(args.apply),
            dry_run: outcome.dry_run,
            root: outcome.root.display().to_string(),
            removed: outcome.removed,
            file_count: outcome.file_count,
            directory_count: outcome.directory_count,
            size_bytes: outcome.size_bytes,
            paths: outcome.paths,
        }),
        0,
    ))
}

pub fn cleanup_persisted(args: RunsArtifactCleanupPersistedArgs) -> CmdResult<RunsOutput> {
    let outcome = runs_service::cleanup_persisted_artifacts(PersistedArtifactCleanupOptions {
        apply: args.apply,
        older_than_days: args.older_than_days,
        run_id: args.run_id,
        kind: args.kind,
        artifact_type: args.artifact_type,
        run_kind: args.run_kind,
        component_id: args.component_id,
        limit: args.limit,
        terminal_only: true,
    })?;

    Ok((
        RunsOutput::ArtifactCleanupPersisted(RunsArtifactCleanupPersistedOutput {
            command: "runs.artifact.cleanup-persisted",
            dry_run: outcome.dry_run,
            artifact_root: outcome.artifact_root.display().to_string(),
            older_than_days: outcome.older_than_days,
            inspected_count: outcome.totals.inspected_count,
            planned_record_count: outcome.planned_record_count,
            planned_file_count: outcome.planned_file_count,
            planned_directory_count: outcome.planned_directory_count,
            planned_size_bytes: outcome.totals.planned_size_bytes,
            removed_record_count: outcome.removed_record_count,
            removed_file_count: outcome.removed_file_count,
            removed_directory_count: outcome.removed_directory_count,
            removed_size_bytes: outcome.totals.removed_size_bytes,
            skipped_count: outcome.skipped_count,
            rows: outcome.rows,
        }),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use homeboy::core::{
        agent_tasks::lifecycle as agent_task_lifecycle,
        observation::{NewRunRecord, ObservationStore, RunStatus},
    };
    use homeboy::test_support::with_isolated_home;

    use super::*;

    fn artifact_root_test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn attach_records_local_runner_file_under_workspace_root() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root.clone()));
            let workspace = home.path().join("runner-workspace");
            fs::create_dir_all(&workspace).expect("workspace");
            let source = workspace.join("report.json");
            fs::write(&source, br#"{"ok":true}"#).expect("source");

            runner::create(
                &format!(
                    r#"{{"id":"issue-6403-local","kind":"local","workspace_root":"{}"}}"#,
                    workspace.display()
                ),
                false,
            )
            .expect("runner");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("runner-exec")
                        .command("homeboy runner exec issue-6403-local")
                        .metadata(serde_json::json!({ "issue": 6403 }))
                        .build(),
                )
                .expect("run");

            let output = attach(RunsArtifactAttachArgs {
                run_id: run.id.clone(),
                runner: "issue-6403-local".to_string(),
                path: source.display().to_string(),
                name: "runner-report".to_string(),
            })
            .expect("attach")
            .0;
            let RunsOutput::ArtifactAttach(output) = output else {
                panic!("unexpected output");
            };

            assert_eq!(output.command, "runs.artifact.attach");
            assert_eq!(output.run_id, run.id);
            assert_eq!(output.artifact.kind, "runner-report");
            assert_eq!(
                output.artifact.metadata_json["runner_id"],
                "issue-6403-local"
            );
            assert_eq!(
                output.artifact.metadata_json["runner_path"],
                source.display().to_string()
            );
            assert!(output.artifact.metadata_json.get("agent_task").is_none());
            assert_eq!(
                output.artifact.metadata_json["resource_lifecycle"]["cleanup_policy"],
                "delete_after_ttl"
            );
            assert_eq!(
                output.artifact.metadata_json["resource_lifecycle"]["evidence_retention"],
                "full"
            );
            assert_eq!(
                output.artifact.metadata_json["resource_lifecycle"]["path"],
                output.artifact.path
            );
            assert!(PathBuf::from(&output.artifact.path).exists());
            let artifacts = store.list_artifacts(&run.id).expect("artifacts");
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].id, output.artifact.id);
        });
    }

    #[test]
    fn attach_records_local_runner_directory_and_preview_entrypoints() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root.clone()));
            let workspace = home.path().join("runner-workspace");
            let site = workspace.join("site-output");
            fs::create_dir_all(site.join("case-a")).expect("site dirs");
            fs::write(site.join("index.html"), b"<html>Home</html>").expect("index");
            fs::write(site.join("case-a/index.html"), b"<html>Case</html>").expect("case");

            runner::create(
                &format!(
                    r#"{{"id":"issue-6462-local","kind":"local","workspace_root":"{}"}}"#,
                    workspace.display()
                ),
                false,
            )
            .expect("runner");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("runner-exec")
                        .command("homeboy runner exec issue-6462-local")
                        .metadata(serde_json::json!({ "issue": 6462 }))
                        .build(),
                )
                .expect("run");

            let output = attach(RunsArtifactAttachArgs {
                run_id: run.id.clone(),
                runner: "issue-6462-local".to_string(),
                path: site.display().to_string(),
                name: "generated-site".to_string(),
            })
            .expect("attach")
            .0;
            let RunsOutput::ArtifactAttach(output) = output else {
                panic!("unexpected output");
            };

            assert_eq!(output.artifact.kind, "generated-site");
            assert_eq!(output.artifact.artifact_type, "directory");
            assert_eq!(output.artifact.metadata_json["source_type"], "directory");
            assert_eq!(
                output.artifact.metadata_json["role"],
                "static_site_artifact"
            );
            let stored_path = PathBuf::from(&output.artifact.path);
            assert!(stored_path.join("index.html").exists());
            assert!(stored_path.join("case-a/index.html").exists());

            let (artifacts_output, _) =
                super::super::handlers::artifacts(&run.id).expect("artifacts output");
            let RunsOutput::Artifacts(artifacts_output) = artifacts_output else {
                panic!("expected artifacts output");
            };
            assert!(artifacts_output
                .preview_entrypoints
                .iter()
                .any(|entrypoint| entrypoint.path == "index.html"));
            assert!(artifacts_output
                .preview_entrypoints
                .iter()
                .any(|entrypoint| entrypoint.path == "case-a/index.html"));
            let lifecycle_index = artifacts_output
                .resource_lifecycle_index
                .expect("resource lifecycle index");
            assert_eq!(lifecycle_index.resources.len(), 1);
            assert_eq!(lifecycle_index.resources[0].run_id, run.id);
            assert_eq!(
                lifecycle_index.resources[0].runner_id.as_deref(),
                Some("issue-6462-local")
            );
            assert_eq!(lifecycle_index.resources[0].kind, "artifact_directory");
        });
    }

    #[test]
    fn attach_rejects_local_path_outside_allowed_roots() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            let workspace = home.path().join("runner-workspace");
            let outside = home.path().join("outside.txt");
            fs::create_dir_all(&workspace).expect("workspace");
            fs::write(&outside, b"outside").expect("outside");
            runner::create(
                &format!(
                    r#"{{"id":"issue-6403-guard","kind":"local","workspace_root":"{}"}}"#,
                    workspace.display()
                ),
                false,
            )
            .expect("runner");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("runner-exec").build())
                .expect("run");

            let result = attach(RunsArtifactAttachArgs {
                run_id: run.id,
                runner: "issue-6403-guard".to_string(),
                path: outside.display().to_string(),
                name: "outside".to_string(),
            });
            let Err(err) = result else {
                panic!("outside path should fail");
            };

            assert!(err
                .to_string()
                .contains("allowed runner workspace/output root"));
        });
    }

    #[test]
    fn attach_rejects_parent_dir_components() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            let workspace = home.path().join("runner-workspace");
            fs::create_dir_all(&workspace).expect("workspace");
            runner::create(
                &format!(
                    r#"{{"id":"issue-6403-parent","kind":"local","workspace_root":"{}"}}"#,
                    workspace.display()
                ),
                false,
            )
            .expect("runner");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("runner-exec").build())
                .expect("run");

            let result = attach(RunsArtifactAttachArgs {
                run_id: run.id,
                runner: "issue-6403-parent".to_string(),
                path: workspace.join("../outside.txt").display().to_string(),
                name: "outside".to_string(),
            });
            let Err(err) = result else {
                panic!("parent-dir path should fail");
            };

            assert!(err.to_string().contains("parent directory"));
        });
    }

    #[test]
    fn preview_serves_directory_artifact_and_surfaces_entrypoints() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("runner-exec").build())
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Pass, None)
                .expect("finish run");
            let site = home.path().join("site");
            fs::create_dir_all(site.join("case-a")).expect("site");
            fs::write(site.join("index.html"), b"<html>Home</html>").expect("index");
            fs::write(site.join("case-a/index.html"), b"<html>Case</html>").expect("case");
            let artifact = store
                .record_directory_artifact_with_metadata(
                    &run.id,
                    "generated_site",
                    &site,
                    serde_json::json!({
                        "entrypoints": [{
                            "path": "index.html",
                            "label": "Open generated homepage",
                            "mime_type": "text/html"
                        }]
                    }),
                )
                .expect("artifact");

            let output = preview(RunsArtifactPreviewArgs {
                run_id: run.id.clone(),
                artifact_id: artifact.id.clone(),
                port: None,
            })
            .expect("preview")
            .0;
            let RunsOutput::ArtifactPreview(output) = output else {
                panic!("unexpected output");
            };
            unsafe {
                libc::kill(output.process_id as i32, libc::SIGTERM);
            }

            assert_eq!(output.command, "runs.artifact.preview");
            assert_eq!(output.run_id, run.id);
            assert_eq!(output.artifact_id, artifact.id);
            assert!(output.base_url.starts_with("http://127.0.0.1:"));
            assert!(output.stop_hint.contains(&output.process_id.to_string()));
            assert!(output.entrypoints.iter().any(|entrypoint| {
                entrypoint.path == "index.html"
                    && entrypoint.public_url.as_deref()
                        == Some(format!("{}index.html", output.base_url).as_str())
            }));
            assert!(output
                .entrypoints
                .iter()
                .any(|entrypoint| entrypoint.path == "case-a/index.html"));
        });
    }

    #[test]
    fn preview_rejects_file_artifacts() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("runner-exec").build())
                .expect("run");
            let file = home.path().join("summary.html");
            fs::write(&file, b"<html></html>").expect("file");
            let artifact = store
                .record_artifact(&run.id, "summary", &file)
                .expect("artifact");

            let result = preview(RunsArtifactPreviewArgs {
                run_id: run.id,
                artifact_id: artifact.id,
                port: None,
            });
            let Err(err) = result else {
                panic!("file artifact should fail");
            };

            assert!(err.to_string().contains("not a previewable directory"));
        });
    }

    #[test]
    fn capture_records_manifest_with_configured_provider() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            let bin = home.path().join("bin");
            fs::create_dir_all(&bin).expect("bin");
            let fake_provider = bin.join("capture-provider");
            fs::write(
                &fake_provider,
                b"#!/bin/sh\nprintf fake-screenshot > \"$HOMEBOY_ARTIFACT_CAPTURE_OUTPUT\"\n",
            )
            .expect("fake provider");
            let mut perms = fs::metadata(&fake_provider)
                .expect("metadata")
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&fake_provider, perms).expect("chmod");
            let old_provider = env::var("HOMEBOY_ARTIFACT_CAPTURE_COMMAND").ok();
            env::set_var(
                "HOMEBOY_ARTIFACT_CAPTURE_COMMAND",
                fake_provider.display().to_string(),
            );

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("runner-exec").build())
                .expect("run");
            let site = home.path().join("site");
            fs::create_dir_all(site.join("fse-pilot-build-theme")).expect("site");
            fs::write(
                site.join("fse-pilot-build-theme/index.html"),
                b"<html>Generated</html>",
            )
            .expect("html");
            let artifact = store
                .record_directory_artifact(&run.id, "generated_site", &site)
                .expect("artifact");
            let output_dir = home.path().join("captures");

            let result = capture(RunsArtifactCaptureArgs {
                run_id: run.id.clone(),
                artifact_id: artifact.id.clone(),
                entrypoints: vec!["fse-pilot-build-theme/index.html".to_string()],
                output_dir: output_dir.clone(),
                port: None,
                viewport_width: 390,
                viewport_height: 844,
            })
            .expect("capture");
            match old_provider {
                Some(value) => env::set_var("HOMEBOY_ARTIFACT_CAPTURE_COMMAND", value),
                None => env::remove_var("HOMEBOY_ARTIFACT_CAPTURE_COMMAND"),
            }
            assert_eq!(result.1, 0);
            let RunsOutput::ArtifactCapture(output) = result.0 else {
                panic!("unexpected output");
            };

            assert_eq!(output.command, "runs.artifact.capture");
            assert_eq!(output.run_id, run.id);
            assert_eq!(output.artifact_id, artifact.id);
            assert!(output.browser.available);
            assert_eq!(output.pages.len(), 1);
            assert_eq!(output.pages[0].status, "captured");
            assert_eq!(output.pages[0].viewport.width, 390);
            assert!(PathBuf::from(&output.pages[0].screenshot_path).exists());
            assert!(PathBuf::from(&output.manifest_path).exists());
            let manifest = fs::read_to_string(&output.manifest_path).expect("manifest");
            assert!(manifest.contains("runs.artifact.capture"));
            assert!(manifest.contains("fse-pilot-build-theme/index.html"));
        });
    }

    #[test]
    fn capture_reports_missing_provider_without_browser_assumptions() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let old_provider = env::var("HOMEBOY_ARTIFACT_CAPTURE_COMMAND").ok();
            env::remove_var("HOMEBOY_ARTIFACT_CAPTURE_COMMAND");
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("runner-exec").build())
                .expect("run");
            let site = home.path().join("site");
            fs::create_dir_all(&site).expect("site");
            fs::write(site.join("index.html"), b"<html>Generated</html>").expect("html");
            let artifact = store
                .record_directory_artifact(&run.id, "generated_site", &site)
                .expect("artifact");

            let result = capture(RunsArtifactCaptureArgs {
                run_id: run.id,
                artifact_id: artifact.id,
                entrypoints: vec!["index.html".to_string()],
                output_dir: home.path().join("captures"),
                port: None,
                viewport_width: 390,
                viewport_height: 844,
            })
            .expect("capture manifest");
            match old_provider {
                Some(value) => env::set_var("HOMEBOY_ARTIFACT_CAPTURE_COMMAND", value),
                None => env::remove_var("HOMEBOY_ARTIFACT_CAPTURE_COMMAND"),
            }
            assert_eq!(result.1, 1);
            let RunsOutput::ArtifactCapture(output) = result.0 else {
                panic!("unexpected output");
            };
            assert!(!output.browser.available);
            assert!(output
                .browser
                .command
                .contains("HOMEBOY_ARTIFACT_CAPTURE_COMMAND"));
            assert!(output
                .browser
                .error
                .unwrap()
                .contains("no browser capture provider configured"));
        });
    }

    #[test]
    fn cleanup_downloads_plans_and_removes_runner_cache() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root.clone()));

            let run_dir = artifact_root.join("runner").join("local").join("run-1");
            fs::create_dir_all(&run_dir).expect("run dir");
            fs::write(run_dir.join("trace.zip"), b"trace").expect("trace");
            fs::write(run_dir.join("report.json"), b"{}").expect("report");

            let dry = cleanup_downloads(RunsArtifactCleanupDownloadsArgs {
                apply: false,
                runner: Some("local".to_string()),
                run_id: Some("run-1".to_string()),
            })
            .expect("dry-run")
            .0;
            let RunsOutput::ArtifactCleanupDownloads(dry) = dry else {
                panic!("unexpected output");
            };
            assert!(dry.dry_run);
            assert_eq!(dry.command, "runs.artifact.cleanup-downloads");
            assert_eq!(dry.cleanup_category, RUNNER_DOWNLOADS_METADATA.category);
            assert_eq!(
                dry.specialist_cleanup_command,
                RUNNER_DOWNLOADS_METADATA.specialist_command(false)
            );
            assert_eq!(
                dry.canonical_cleanup_command,
                RUNNER_DOWNLOADS_METADATA.canonical_cleanup_command(false)
            );
            assert!(!dry.removed);
            assert_eq!(dry.file_count, 2);
            assert_eq!(dry.directory_count, 0);
            assert_eq!(dry.size_bytes, 7);
            assert!(run_dir.exists());

            let (inventory, _) = crate::commands::cleanup::run(
                crate::commands::cleanup::CleanupArgs {
                    apply: false,
                    include: vec![crate::commands::cleanup::CleanupCategoryArg::RunnerDownloads],
                    exclude: Vec::new(),
                    older_than_days: None,
                    limit: None,
                    command: None,
                },
                &crate::commands::GlobalArgs {},
            )
            .expect("canonical cleanup inventory");
            assert_eq!(inventory["category_count"], 1);
            assert_eq!(inventory["categories"][0]["category"], dry.cleanup_category);
            assert_eq!(
                inventory["categories"][0]["canonical_cleanup_command"],
                dry.canonical_cleanup_command
            );
            assert_eq!(
                inventory["categories"][0]["specialist_command"],
                dry.specialist_cleanup_command
            );

            let applied = cleanup_downloads(RunsArtifactCleanupDownloadsArgs {
                apply: true,
                runner: Some("local".to_string()),
                run_id: Some("run-1".to_string()),
            })
            .expect("apply")
            .0;
            let RunsOutput::ArtifactCleanupDownloads(applied) = applied else {
                panic!("unexpected output");
            };
            assert!(!applied.dry_run);
            assert_eq!(
                applied.canonical_cleanup_command,
                RUNNER_DOWNLOADS_METADATA.canonical_cleanup_command(true)
            );
            assert_eq!(
                applied.specialist_cleanup_command,
                RUNNER_DOWNLOADS_METADATA.specialist_command(true)
            );
            assert!(applied.removed);
            assert_eq!(applied.file_count, 2);
            assert_eq!(applied.size_bytes, 7);
            assert!(!run_dir.exists());
        });
    }

    #[test]
    fn cleanup_persisted_plans_and_removes_local_artifacts() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root.clone()));

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("bench")
                        .component_id("homeboy")
                        .command("homeboy bench")
                        .metadata(serde_json::json!({ "test": true }))
                        .build(),
                )
                .expect("run");
            let source = home.path().join("bench.json");
            fs::write(&source, b"bench").expect("source");
            let artifact = store
                .record_artifact(&run.id, "summary", &source)
                .expect("artifact");
            let stored_path = PathBuf::from(&artifact.path);
            assert!(stored_path.exists());

            // The retention contract never reaps evidence while its run owns an
            // active lease, even when the age threshold is zero.
            let active = cleanup_persisted(RunsArtifactCleanupPersistedArgs {
                apply: true,
                older_than_days: 0,
                run_id: Some(run.id.clone()),
                kind: None,
                artifact_type: None,
                run_kind: None,
                component_id: None,
                limit: 100,
            })
            .expect("active cleanup")
            .0;
            let RunsOutput::ArtifactCleanupPersisted(active) = active else {
                panic!("unexpected output");
            };
            assert_eq!(active.removed_record_count, 0);
            assert_eq!(active.rows[0].run_status, "running");
            assert_eq!(active.rows[0].action, "skip");
            assert!(stored_path.exists());

            store
                .finish_run(&run.id, RunStatus::Pass, None)
                .expect("finish run");

            let dry = cleanup_persisted(RunsArtifactCleanupPersistedArgs {
                apply: false,
                older_than_days: 0,
                run_id: Some(run.id.clone()),
                kind: None,
                artifact_type: None,
                run_kind: None,
                component_id: None,
                limit: 100,
            })
            .expect("dry-run")
            .0;
            let RunsOutput::ArtifactCleanupPersisted(dry) = dry else {
                panic!("unexpected output");
            };
            assert!(dry.dry_run);
            assert_eq!(dry.planned_record_count, 1);
            assert_eq!(dry.planned_file_count, 1);
            assert_eq!(dry.planned_size_bytes, 5);
            assert!(stored_path.exists());
            assert!(store.get_artifact(&artifact.id).expect("get").is_some());

            let applied = cleanup_persisted(RunsArtifactCleanupPersistedArgs {
                apply: true,
                older_than_days: 0,
                run_id: Some(run.id.clone()),
                kind: None,
                artifact_type: None,
                run_kind: None,
                component_id: None,
                limit: 100,
            })
            .expect("apply")
            .0;
            let RunsOutput::ArtifactCleanupPersisted(applied) = applied else {
                panic!("unexpected output");
            };
            assert!(!applied.dry_run);
            assert_eq!(applied.removed_record_count, 1);
            assert_eq!(applied.removed_file_count, 1);
            assert!(!stored_path.exists());
            assert!(store.get_artifact(&artifact.id).expect("get").is_none());
        });
    }

    #[test]
    fn cleanup_downloads_requires_runner_for_run_filter() {
        let result = cleanup_downloads(RunsArtifactCleanupDownloadsArgs {
            apply: false,
            runner: None,
            run_id: Some("run-1".to_string()),
        });
        let Err(err) = result else {
            panic!("missing runner should fail");
        };

        assert!(err.to_string().contains("--run-id requires --runner"));
    }

    #[test]
    fn cleanup_downloads_rejects_path_traversal_filters() {
        for (runner, run_id) in [
            (Some("../outside".to_string()), None),
            (Some("local".to_string()), Some("../outside".to_string())),
            (Some("/tmp/outside".to_string()), None),
        ] {
            let result = cleanup_downloads(RunsArtifactCleanupDownloadsArgs {
                apply: false,
                runner,
                run_id,
            });
            let Err(err) = result else {
                panic!("path traversal should fail");
            };

            assert!(err.to_string().contains("single path component"));
        }
    }
}
