use super::protocol::*;
use super::*;
use std::process::Command;

/// All roots belonging to one source-materialization stage are admitted and
/// installed together. Non-snapshot callers retain their checkout semantics.
pub(crate) fn sync_workspace_batch_controlled(
    runner_id: &str,
    requests: Vec<(RunnerWorkspaceSyncOptions, Vec<String>)>,
    control: &WorkspaceControl,
) -> Result<Vec<RunnerWorkspaceSyncOutput>> {
    control.checkpoint()?;
    let roots = homeboy_core::paths::PathRoots::from_environment()?;
    let runner = load_in_roots(&roots, runner_id)?;
    sync_workspace_batch_in_roots(&roots, &runner, requests, control)
}

pub(super) fn sync_workspace_batch_in_roots(
    roots: &homeboy_core::paths::PathRoots,
    runner: &Runner,
    requests: Vec<(RunnerWorkspaceSyncOptions, Vec<String>)>,
    control: &WorkspaceControl,
) -> Result<Vec<RunnerWorkspaceSyncOutput>> {
    let runner_id = runner.id.as_str();
    let workspace_root = runner
        .workspace_root
        .as_deref()
        .ok_or_else(|| io("runner has no workspace root"))?;
    validate_absolute_path("workspace_root", workspace_root)?;
    let local_transport = runner.kind == RunnerKind::Local
        || super::super::util::ssh_client_for_runner(runner)
            .is_ok_and(|(_, client)| client.is_local);
    let transfer = if local_transport {
        fs::create_dir_all(workspace_root).map_err(io)?;
        tempfile::tempdir_in(workspace_root).map_err(io)?
    } else {
        tempfile::tempdir().map_err(io)?
    };
    let mut requests = requests;
    let original_count = requests.len();
    let mut dependencies = Vec::new();
    for parent in 0..original_count {
        let local = canonical_workspace_path(&requests[parent].0.path)?;
        let mut inherited = super::super::types::default_excludes_for(&local);
        inherited.extend(runner.policy.snapshot_excludes.clone());
        inherited.extend(requests[parent].1.clone());
        for dependency in crate::validation_dependencies::validation_dependency_workspaces(
            &local,
            &inherited,
            requests[parent].0.validation_dependency_ids.as_deref(),
        )? {
            let index = requests.len();
            requests.push((
                RunnerWorkspaceSyncOptions {
                    path: dependency.prepared_path.display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Snapshot,
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                    allow_dirty_lab_workspace: false,
                    validation_dependency_ids: Some(Vec::new()),
                    run_isolation_token: None,
                },
                dependency.snapshot_excludes.clone(),
            ));
            dependencies.push((parent, index, dependency));
        }
    }
    let mut outputs = Vec::new();
    let mut summaries = Vec::new();
    let mut commit_roots = Vec::new();
    let mut git_sources = Vec::new();
    let mut admissions = Vec::new();
    let mut sources = std::collections::BTreeMap::new();
    for (index, (options, extra_excludes)) in requests.iter().enumerate() {
        let local_path = canonical_workspace_path(&options.path)?;
        let mut excludes = super::super::types::default_excludes_for(&local_path);
        for pattern in runner
            .policy
            .snapshot_excludes
            .iter()
            .chain(
                homeboy_core::source_snapshot::policy_for_path(&local_path)
                    .sync_excludes
                    .iter(),
            )
            .chain(extra_excludes)
        {
            if !excludes.contains(pattern) {
                excludes.push(pattern.clone());
            }
        }
        let mut includes = runner.policy.snapshot_includes.clone();
        for pattern in &options.snapshot_includes {
            if !includes.contains(pattern) {
                includes.push(pattern.clone());
            }
        }
        let excludes = effective_snapshot_excludes(excludes, &includes);
        let (snapshot, stats, manifest, digest) =
            snapshot_content_controlled(&local_path, &excludes, &includes, control)?;
        let remote_path = temp::unique_name(
            &deterministic_remote_path(
                workspace_root,
                &local_path,
                &snapshot,
                options.run_isolation_token.as_deref(),
            ),
            "",
        );
        for entry in &manifest.entries {
            if entry.kind == "file" {
                let digest = entry
                    .sha256
                    .clone()
                    .ok_or_else(|| io("file has no digest"))?;
                sources
                    .entry(digest.clone())
                    .or_insert(super::blobs::Source::new(
                        local_path.join(&entry.path),
                        digest,
                    )?);
            }
        }
        let directory = transfer.path().join(index.to_string());
        fs::create_dir(&directory).map_err(io)?;
        let head = if options.mode == RunnerWorkspaceSyncMode::SnapshotGit {
            control.git(&local_path, &["rev-parse", "HEAD"]).ok()
        } else {
            None
        };
        let remote_url = control
            .git(&local_path, &["config", "--get", "remote.origin.url"])
            .unwrap_or_else(|_| "homeboy-controller-bundle".into());
        let cache = super::super::git::git_object_cache_path(&remote_path, &remote_url);
        let summary = RootSummary {
            remote_path: remote_path.clone(),
            object_cache: cache,
            manifest: manifest.clone(),
            content_hash: digest.clone(),
        };
        summaries.push(RootSummary {
            remote_path: remote_path.clone(),
            object_cache: summary.object_cache.clone(),
            manifest: manifest.clone(),
            content_hash: digest.clone(),
        });
        let cleanliness = if head.is_some() {
            "snapshot_synthetic_git_unique_workspace"
        } else {
            "snapshot_unique_workspace"
        };
        let mut plan = workspace_materialization_plan(
            workspace_root,
            &local_path,
            &remote_path,
            &snapshot,
            options,
            &includes,
            cleanliness,
        );
        plan.actual_materialization_mode = Some(options.mode.as_str().into());
        if options.mode == RunnerWorkspaceSyncMode::Snapshot {
            plan.actual_materialization_mode = Some("filesystem_snapshot".into());
            plan.snapshot_transfer = Some(super::super::types::SnapshotTransferStats {
                missing_blobs: 0,
                missing_bytes: 0,
                total_blobs: 0,
                total_bytes: 0,
                reused: ByteFileCounts::default(),
                transferred: stats,
                final_size: stats,
            });
        }
        let metadata = workspace_metadata(WorkspaceMetadataRequest {
            runner_id,
            local_path: &local_path,
            remote_path: &remote_path,
            sync_mode: options.mode,
            actual_materialization_mode: plan.actual_materialization_mode.as_deref(),
            fallback_reason: None,
            source_transfer: None,
            snapshot_identity: &snapshot,
            snapshot_excludes: &excludes,
            content_manifest: Some(manifest.clone()),
            run_id: options.run_isolation_token.as_deref(),
            cleanup_policy: ResourceCleanupPolicy::DeleteOnSuccess,
        });
        let prepared_git = options.mode == RunnerWorkspaceSyncMode::SnapshotGit && head.is_none();
        let synthetic = None;
        let current_workspace =
            current_workspace_summary(&local_path, &remote_path, options.mode, true, synthetic);
        let workspace_lease = workspace_lease(runner_id, &current_workspace);
        outputs.push(RunnerWorkspaceSyncOutput {
            variant: "workspace_sync",
            command: "runner.workspace.sync",
            runner_id: runner_id.into(),
            local_path: local_path.display().to_string(),
            remote_path,
            materialization_plan: plan,
            current_workspace,
            workspace_lease,
            resource_lifecycle: metadata
                .resource_lifecycle
                .clone()
                .expect("new workspace lifecycle"),
            sync_mode: options.mode,
            snapshot_identity: snapshot,
            content: Some((manifest, digest)),
            workspace_ref: metadata.workspace_lease.clone().expect("new workspace ref"),
            prepared_workspace_lease: metadata.workspace_lease.clone(),
            counts: stats,
            excludes,
            includes,
            workspace_cleanliness: cleanliness.into(),
            validation_dependencies: Vec::new(),
        });
        commit_roots.push(CommitRoot {
            summary,
            metadata,
            install_git: None,
            prepared_git,
            source_evidence: None,
        });
        git_sources.push((head, remote_url));
    }
    for (parent, index, dependency) in &dependencies {
        let remote_path = format!(
            "{}/{}",
            parent_remote_path(&outputs[*parent].remote_path),
            sanitize_path_segment(&dependency.remote_name)
        );
        outputs[*parent]
            .validation_dependencies
            .push(RunnerValidationDependencySyncOutput {
                id: dependency.remote_name.clone(),
                role: "validation_dependency".into(),
                local_path: dependency.local_path.display().to_string(),
                remote_path: remote_path.clone(),
                prepare_cache: Some(dependency.prepared.outcome.as_str().into()),
            });
        summaries[*index].remote_path = remote_path.clone();
        commit_roots[*index].summary.remote_path = remote_path.clone();
        commit_roots[*index].metadata.remote_path = remote_path;
        commit_roots[*index].source_evidence = Some(dependency.source_evidence.clone());
    }
    let plan = PlanRequest {
        identity: crate::controller_identity::compatibility_controller_identity().display,
        roots: summaries,
    };
    write_json(&transfer.path().join("plan.json"), &plan)?;
    let response = homeboy_core::observation::phase_timing::time(
        "stage.lab.source_materialization.plan",
        || invoke(runner, transfer.path(), None, control),
    )?;
    let response: PlanResponse = serde_json::from_value(response).map_err(io)?;
    if response.identity != plan.identity {
        return Err(io(format!(
            "runner build identity mismatch: requested {}, received {}",
            plan.identity, response.identity
        )));
    }
    let inventory = super::blobs::inventory(&plan.roots)?;
    let missing_bytes: u64 = response
        .missing
        .iter()
        .map(|d| {
            inventory
                .get(d)
                .copied()
                .ok_or_else(|| io("plan requested unknown digest"))
        })
        .collect::<Result<Vec<_>>>()?
        .iter()
        .sum();
    for output in &mut outputs {
        let stats = super::super::types::SnapshotTransferStats {
            missing_blobs: response.missing.len() as u64,
            missing_bytes,
            total_blobs: inventory.len() as u64,
            total_bytes: inventory.values().sum(),
            reused: ByteFileCounts {
                files: inventory.len().saturating_sub(response.missing.len()),
                bytes: inventory
                    .values()
                    .sum::<u64>()
                    .saturating_sub(missing_bytes),
            },
            transferred: ByteFileCounts {
                files: response.missing.len(),
                bytes: missing_bytes,
            },
            final_size: output.counts,
        };
        output.materialization_plan.snapshot_transfer = Some(stats);
    }
    let mut requirement = SnapshotFilesystemRequirement {
        bytes: 0,
        inodes: 0,
    };
    for root in &plan.roots {
        let root_requirement = super::blobs::assembled_size(root);
        requirement.bytes = requirement.bytes.saturating_add(root_requirement.bytes);
        requirement.inodes = requirement.inodes.saturating_add(root_requirement.inodes);
    }
    requirement.bytes = requirement.bytes.saturating_add(missing_bytes);
    requirement.inodes += response.missing.len() as u64;
    let probe = SnapshotFilesystemProbe {
        identity: format!("runner:{runner_id}:{workspace_root}"),
        path: workspace_root.into(),
        role: "runner snapshot destination",
        available_bytes: response.available_bytes,
        available_inodes: response.available_inodes,
    };
    if runner_workspace_disk_is_critical(RunnerWorkspaceDiskProbe {
        available_bytes: response.available_bytes,
        total_bytes: response.total_bytes,
    }) || !snapshot_probe_has_capacity(&probe, requirement)
    {
        return Err(snapshot_capacity_error(&probe, requirement, runner, None));
    }
    admissions.push(SnapshotFilesystemAdmission::acquire(
        roots.data(),
        transfer.path().to_path_buf(),
        &[probe],
        requirement,
        runner,
    )?);
    for (index, ((head, remote_url), root)) in git_sources
        .into_iter()
        .zip(commit_roots.iter_mut())
        .enumerate()
    {
        let Some(head) = head else { continue };
        let local_path = Path::new(&outputs[index].local_path);
        let options = &requests[index].0;
        let refs = super::super::git::controller_bundle_refs("HEAD", None, &options.git_fetch_refs);
        super::super::git::validate_controller_git_bundle_source(local_path, control)?;
        homeboy_core::observation::phase_timing::time(
            "stage.lab.source_materialization.bundle_object_closure",
            || {
                super::super::git::hydrate_controller_bundle_objects_controlled(
                    local_path, &refs, control,
                )
            },
        )?;
        let cached = response
            .tips
            .get(index)
            .ok_or_else(|| io("plan root count mismatch"))?;
        let prerequisites = homeboy_core::observation::phase_timing::time(
            "stage.lab.source_materialization.delta_computation",
            || super::super::git::bundle_prerequisites(local_path, &head, cached, control),
        )?;
        let bundle = transfer
            .path()
            .join(index.to_string())
            .join("workspace.bundle");
        let output = homeboy_core::observation::phase_timing::time(
            "stage.lab.source_materialization.bundle_build",
            || {
                control.output(
                    Command::new("git")
                        .args(["bundle", "create"])
                        .arg(&bundle)
                        .args(&refs)
                        .args(prerequisites.iter().map(|commit| format!("^{commit}")))
                        .env("GIT_NO_LAZY_FETCH", "1")
                        .current_dir(local_path),
                    "build batch Git bundle",
                )
            },
        )?;
        if !output.status.success() {
            return Err(io(String::from_utf8_lossy(&output.stderr)));
        }
        let digest = homeboy_core::observation::phase_timing::time(
            "stage.lab.source_materialization.bundle_hash",
            || super::super::snapshot::snapshot_file_sha256(&bundle, control),
        )?;
        let retain = super::super::git::bundled_ref_commits(local_path, &refs, control)?;
        let branch = control
            .git(local_path, &["symbolic-ref", "--short", "HEAD"])
            .ok();
        root.install_git = Some(GitInstall {
            head: head.clone(),
            branch,
            remote_url,
            sha256: digest.clone(),
            prerequisites: prerequisites.clone(),
            retain,
        });
        let provenance = super::super::types::ControllerGitBundleProvenance {
            provenance: "controller_git_bundle",
            source_sha: head,
            source_refs: refs,
            prerequisites,
            sha256: digest,
            cleanup_owner: "runner",
            cleanup_ttl: "PT0S",
        };
        root.metadata.source_transfer = Some(serde_json::to_value(&provenance).map_err(io)?);
        outputs[index].materialization_plan.controller_git_bundle = Some(provenance);
    }
    write_json(
        &transfer.path().join("commit.json"),
        &CommitRequest {
            identity: response.identity,
            roots: commit_roots,
        },
    )?;
    let committed = homeboy_core::observation::phase_timing::time(
        "stage.lab.source_materialization.commit",
        || {
            homeboy_core::observation::phase_timing::time(
                "stage.lab.source_materialization.transfer_apply_verify",
                || {
                    invoke(
                        runner,
                        transfer.path(),
                        Some((&sources, &response.missing)),
                        control,
                    )
                },
            )
        },
    )
    .map_err(|mut error| {
        error.details["workspace_effect"] = serde_json::json!({
            "runner_id": runner_id, "status": "commit_interrupted_or_unobserved",
            "roots": plan.roots.iter().map(|root| &root.remote_path).collect::<Vec<_>>(),
        });
        error
    })?;
    control.checkpoint()?;
    for (index, output) in outputs.iter_mut().enumerate() {
        if let Some(value) = committed
            .get("synthetic")
            .and_then(|v| v.get(index))
            .filter(|v| !v.is_null())
        {
            output.current_workspace.synthetic_checkout_commit =
                value["synthetic_commit"].as_str().map(str::to_string);
            output.current_workspace.synthetic_checkout_ref =
                value["synthetic_ref"].as_str().map(str::to_string);
            output.current_workspace.synthetic_checkout_tree =
                value["synthetic_tree"].as_str().map(str::to_string);
        }
    }
    outputs.truncate(original_count);
    Ok(outputs)
}

pub(super) fn invoke(
    runner: &Runner,
    directory: &Path,
    blobs: Option<(
        &std::collections::BTreeMap<String, super::blobs::Source>,
        &[String],
    )>,
    control: &WorkspaceControl,
) -> Result<serde_json::Value> {
    control.checkpoint()?;
    #[cfg(test)]
    INVOCATIONS.with(|counter| counter.set(counter.get() + 1));
    let local = runner.kind == RunnerKind::Local
        || super::super::util::ssh_client_for_runner(runner)
            .is_ok_and(|(_, client)| client.is_local);
    let root = runner
        .workspace_root
        .as_deref()
        .ok_or_else(|| io("runner has no workspace root"))?;
    if local && blobs.is_none() {
        let response = materialization_plan_controlled(&directory.join("plan.json"), control)?;
        control.checkpoint()?;
        return Ok(response);
    }
    let incoming = if local {
        Some(tempfile::tempdir_in(root).map_err(io)?)
    } else {
        None
    };
    let response = super::blobs::pipe(
        |writer| {
            let mut archive = tar::Builder::new(writer);
            if let Some((sources, missing)) = blobs {
                for digest in missing {
                    control.checkpoint()?;
                    sources
                        .get(digest)
                        .ok_or_else(|| io("missing source blob"))?
                        .append(
                            &mut archive,
                            &PathBuf::from("blobs").join(super::blobs::key(digest)?),
                            control,
                        )?;
                }
                for entry in fs::read_dir(directory).map_err(io)? {
                    let entry = entry.map_err(io)?;
                    if entry.file_type().map_err(io)?.is_dir() {
                        archive
                            .append_dir_all(entry.file_name(), entry.path())
                            .map_err(io)?;
                    }
                }
            }
            // The declaration is the seal: never send it after a failed stability
            // check, even when an extractor accepts an otherwise partial archive.
            let name = if blobs.is_some() {
                "commit.json"
            } else {
                "plan.json"
            };
            archive
                .append_path_with_name(directory.join(name), name)
                .map_err(io)?;
            archive.finish().map_err(io)
        },
        |reader| {
            if let Some(incoming) = &incoming {
                tar::Archive::new(reader)
                    .unpack(incoming.path())
                    .map_err(io)?;
                materialization_commit_controlled(incoming.path(), control)
            } else {
                let binary = crate::remote_runner_homeboy_path(runner, "materialize Lab sources")?;
                let remote = materialization_script(root, binary, blobs.is_some());
                let command = super::super::util::shell_command_for_runner(runner, &remote)?;
                let mut output = control
                    .output_with_stdin(
                        Command::new("bash").args(["-o", "pipefail", "-c", &command]),
                        reader,
                        "runner materialization call",
                    )
                    .map_err(|mut error| {
                        if blobs.is_some()
                            && error.details["workspace_sync"]["command_started"] == true
                        {
                            error.details["workspace_sync"]["remote_effect_uncertain"] =
                                serde_json::json!(true);
                        }
                        error
                    })?;
                if !output.status.success() {
                    if output.stdout.is_empty() && output.stderr.is_empty() {
                        output.stderr = format!(
                            "materialization transport exited {} without a remote failure marker; runner={}; operation={}; remote script was not observed (check SSH wrapper and blob_stream_producer evidence)",
                            output.status, runner.id, if blobs.is_some() { "commit" } else { "plan" },
                        ).into_bytes();
                    }
                    return super::super::util::shell_output_failure(output, "runner materialization call").map(|()| serde_json::Value::Null).map_err(|mut error| {
                    if blobs.is_some() && error.code == ErrorCode::RunnerLabTransportFailure { error.details["workspace_sync"] = serde_json::json!({"command_started": true, "remote_effect_uncertain": true}); }
                    error
                });
                }
                json(&output.stdout)
            }
        },
    )?;
    control.checkpoint()?;
    Ok(response)
}

/// The EXIT trap covers setup, extraction, binary execution and response read.
/// Preserve the original status even if evidence collection or cleanup fails.
pub(super) fn materialization_script(root: &str, binary: &str, commit: bool) -> String {
    format!(
        "set -eu; stage=''; phase=setup; trap 'rc=$?; trap - EXIT; if [ \"$rc\" -ne 0 ]; then printf \"materialization failed rc=%s phase=%s stage=%s\\n\" \"$rc\" \"$phase\" \"$stage\" >&2; if [ -n \"$stage\" ]; then ls -la \"$stage\" >&2 || :; if [ -f \"$stage/response.json\" ]; then cat \"$stage/response.json\" >&2 || :; fi; fi; fi; if [ -n \"$stage\" ]; then rm -rf \"$stage\" || :; fi; exit \"$rc\"' EXIT; trap 'exit 129' HUP; trap 'exit 130' INT; trap 'exit 143' TERM; mkdir -p {root}; stage=$(mktemp -d {template}); phase=extract; tar -p -C \"$stage\" -xf -; phase=execute; {binary} runner workspace materialization --input \"$stage{input}\" --response \"$stage/response.json\" {commit} >&2; phase=response; cat \"$stage/response.json\"",
        root = shell::quote_arg(root),
        template = shell::quote_arg(&format!("{root}/.materialization.XXXXXXXX")),
        binary = shell::quote_arg(binary),
        input = if commit { "" } else { "/plan.json" },
        commit = if commit { "--commit" } else { "" },
    )
}

#[cfg(test)]
thread_local! { pub(crate) static INVOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
