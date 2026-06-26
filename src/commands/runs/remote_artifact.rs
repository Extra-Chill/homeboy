use std::fs;
use std::net::TcpListener;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use homeboy::core::engine::shell;
use homeboy::core::observation::runs_service::{
    self, PersistedArtifactCleanupOptions, RunnerDownloadCleanupOptions,
};
use homeboy::core::observation::ArtifactRecord;
use homeboy::core::observation::ObservationStore;
use homeboy::core::runners::{self as runner, Runner, RunnerKind};
use homeboy::core::{server, Error};

use super::types::{
    RunsArtifactAttachArgs, RunsArtifactAttachOutput, RunsArtifactCleanupDownloadsArgs,
    RunsArtifactCleanupDownloadsOutput, RunsArtifactCleanupPersistedArgs,
    RunsArtifactCleanupPersistedOutput, RunsArtifactGetOutput, RunsArtifactPreviewArgs,
    RunsArtifactPreviewOutput, RunsOutput,
};
use super::CmdResult;

pub fn get(artifact: ArtifactRecord, output: Option<PathBuf>) -> CmdResult<RunsOutput> {
    let download = runs_service::download_remote_artifact(artifact, output)?;
    Ok((
        RunsOutput::ArtifactGet(RunsArtifactGetOutput {
            command: "runs.artifact.get",
            run_id: download.run_id,
            artifact_id: download.artifact_id,
            output_path: download.output_path.display().to_string(),
            content_type: download.content_type,
            size_bytes: download.size_bytes,
            sha256: download.sha256,
            artifact_ref: download.artifact_ref,
        }),
        0,
    ))
}

pub fn attach(args: RunsArtifactAttachArgs) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    runs_service::require_run(&store, &args.run_id)?;
    validate_artifact_name(&args.name)?;
    let runner = runner::load(&args.runner)?;
    validate_runner_artifact_path(&runner, &args.path)?;

    let source = copy_runner_artifact_source(&runner, &args.path)?;
    let metadata = runner_attach_metadata(&runner, &args.path, &source);
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
    if source.temporary {
        match source.artifact_type {
            RunnerAttachArtifactType::File => {
                let _ = fs::remove_file(&source.path);
            }
            RunnerAttachArtifactType::Directory => {
                let _ = fs::remove_dir_all(&source.path);
            }
        }
    }

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

    let port = args.port.map(Ok).unwrap_or_else(available_port)?;
    let base_url = format!("http://127.0.0.1:{port}/");
    let port_arg = port.to_string();
    let directory_arg = artifact_path.display().to_string();
    let child = Command::new("python3")
        .args([
            "-m",
            "http.server",
            &port_arg,
            "--bind",
            "127.0.0.1",
            "--directory",
            &directory_arg,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("start python3 static artifact preview server".to_string()),
            )
        })?;
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

fn available_port() -> homeboy::core::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|err| {
        Error::internal_io(err.to_string(), Some("reserve preview port".to_string()))
    })?;
    let port = listener.local_addr().map_err(|err| {
        Error::internal_io(err.to_string(), Some("read preview port".to_string()))
    })?;
    Ok(port.port())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunnerAttachArtifactType {
    File,
    Directory,
}

struct RunnerAttachSource {
    path: PathBuf,
    artifact_type: RunnerAttachArtifactType,
    temporary: bool,
}

fn runner_attach_metadata(
    runner: &Runner,
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
    metadata
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
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return Err(Error::validation_invalid_argument(
            "path",
            "runner artifact attach requires an absolute runner-side path",
            Some(path.to_string()),
            None,
        ));
    }
    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Error::validation_invalid_argument(
            "path",
            "runner artifact attach path must not contain parent directory components",
            Some(path.to_string()),
            None,
        ));
    }

    let roots = allowed_runner_artifact_roots(runner)?;
    if roots.iter().any(|root| path_is_within_root(path, root)) {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "path",
        "runner artifact path must be under an allowed runner workspace/output root",
        Some(path.to_string()),
        Some(vec![format!("Allowed roots: {}", roots.join(", "))]),
    ))
}

fn allowed_runner_artifact_roots(runner: &Runner) -> homeboy::core::Result<Vec<String>> {
    let mut roots = Vec::new();
    if let Some(root) = runner.workspace_root.as_deref() {
        roots.push(normalize_root(root));
    }
    roots.extend(
        runner
            .policy
            .workspace_roots
            .iter()
            .map(|root| normalize_root(root)),
    );
    if let Some(root) = runner.env.get("HOMEBOY_ARTIFACT_ROOT") {
        roots.push(normalize_root(root));
    }
    if runner.kind == RunnerKind::Local {
        roots.push(normalize_root(
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

fn normalize_root(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

fn path_is_within_root(path: &str, root: &str) -> bool {
    let root = normalize_root(root);
    path == root || path.starts_with(&format!("{root}/"))
}

fn copy_runner_artifact_source(
    runner: &Runner,
    path: &str,
) -> homeboy::core::Result<RunnerAttachSource> {
    match runner.kind {
        RunnerKind::Local => {
            let source = PathBuf::from(path);
            let metadata = fs::metadata(&source).map_err(|err| {
                Error::internal_io(err.to_string(), Some(format!("read artifact {path}")))
            })?;
            let artifact_type = if metadata.is_file() {
                RunnerAttachArtifactType::File
            } else if metadata.is_dir() {
                RunnerAttachArtifactType::Directory
            } else {
                return Err(Error::validation_invalid_argument(
                    "path",
                    "runner artifact attach supports files and directories",
                    Some(path.to_string()),
                    None,
                ));
            };
            Ok(RunnerAttachSource {
                path: source,
                artifact_type,
                temporary: false,
            })
        }
        RunnerKind::Ssh => {
            let server_id = runner.server_id.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "runner",
                    "SSH runner is missing server_id",
                    Some(runner.id.clone()),
                    None,
                )
            })?;
            let temp_path = attach_download_path(&runner.id, path)?;
            if let Some(parent) = temp_path.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some(format!("create {}", parent.display())),
                    )
                })?;
            }
            let server = server::load(server_id)?;
            let client = server::SshClient::from_server(&server, server_id)?;
            let artifact_type = remote_runner_artifact_type(&client, path)?;
            if artifact_type == RunnerAttachArtifactType::Directory {
                let output = server::transfer::transfer(&server::transfer::TransferConfig {
                    source: format!("{server_id}:{path}"),
                    destination: temp_path.display().to_string(),
                    recursive: true,
                    compress: true,
                    dry_run: false,
                    exclude: Vec::new(),
                })?;
                if output.1 != 0 || !output.0.success {
                    return Err(Error::validation_invalid_argument(
                        "path",
                        format!(
                            "failed to download runner artifact directory: {}",
                            output.0.error.unwrap_or_else(|| "scp failed".to_string())
                        ),
                        Some(path.to_string()),
                        None,
                    ));
                }
                return Ok(RunnerAttachSource {
                    path: temp_path,
                    artifact_type,
                    temporary: true,
                });
            }
            let output = client.download_file(path, &temp_path.display().to_string());
            if !output.success {
                return Err(Error::validation_invalid_argument(
                    "path",
                    format!(
                        "failed to download runner artifact: {}",
                        output.stderr.trim()
                    ),
                    Some(path.to_string()),
                    None,
                ));
            }
            Ok(RunnerAttachSource {
                path: temp_path,
                artifact_type,
                temporary: true,
            })
        }
    }
}

fn remote_runner_artifact_type(
    client: &server::SshClient,
    path: &str,
) -> homeboy::core::Result<RunnerAttachArtifactType> {
    let quoted = shell::quote_path(path);
    if client.execute(&format!("test -d {quoted}")).success {
        return Ok(RunnerAttachArtifactType::Directory);
    }
    if client.execute(&format!("test -f {quoted}")).success {
        return Ok(RunnerAttachArtifactType::File);
    }
    Err(Error::validation_invalid_argument(
        "path",
        "runner artifact path is not a file or directory",
        Some(path.to_string()),
        None,
    ))
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

fn attach_download_path(runner_id: &str, path: &str) -> homeboy::core::Result<PathBuf> {
    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("artifact");
    Ok(homeboy::core::artifact_root()?
        .join("runner-attach")
        .join(runner_id)
        .join(format!("{}-{file_name}", uuid::Uuid::new_v4())))
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
    })?;

    Ok((
        RunsOutput::ArtifactCleanupPersisted(RunsArtifactCleanupPersistedOutput {
            command: "runs.artifact.cleanup-persisted",
            dry_run: outcome.dry_run,
            artifact_root: outcome.artifact_root.display().to_string(),
            older_than_days: outcome.older_than_days,
            inspected_count: outcome.inspected_count,
            planned_record_count: outcome.planned_record_count,
            planned_file_count: outcome.planned_file_count,
            planned_directory_count: outcome.planned_directory_count,
            planned_size_bytes: outcome.planned_size_bytes,
            removed_record_count: outcome.removed_record_count,
            removed_file_count: outcome.removed_file_count,
            removed_directory_count: outcome.removed_directory_count,
            removed_size_bytes: outcome.removed_size_bytes,
            skipped_count: outcome.skipped_count,
            rows: outcome.rows,
        }),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use homeboy::core::observation::{NewRunRecord, ObservationStore, RunStatus};
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
            assert!(!dry.removed);
            assert_eq!(dry.file_count, 2);
            assert_eq!(dry.directory_count, 0);
            assert_eq!(dry.size_bytes, 7);
            assert!(run_dir.exists());

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
