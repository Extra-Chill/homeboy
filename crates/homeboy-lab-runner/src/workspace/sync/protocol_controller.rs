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
        let (snapshot, stats, manifest, digest) = snapshot_identity_manifest_and_hash_controlled(
            &local_path,
            &excludes,
            &includes,
            control,
        )?;
        let remote_path = temp::unique_name(
            &deterministic_remote_path(
                workspace_root,
                &local_path,
                &snapshot,
                options.run_isolation_token.as_deref(),
            ),
            "",
        );
        // Controller scratch admission is local; runner capacity arrives in plan.
        let requirement = snapshot_filesystem_requirement(stats.bytes, manifest.entry_count as u64);
        let probe =
            local_snapshot_filesystem_probe(transfer.path(), "controller snapshot scratch")?;
        admissions.push(SnapshotFilesystemAdmission::acquire(
            roots.data(),
            transfer.path().to_path_buf(),
            &[probe],
            requirement,
            runner,
        )?);
        let directory = transfer.path().join(index.to_string());
        fs::create_dir(&directory).map_err(io)?;
        let source = directory.join("source");
        // Reuse the established immutable staging and stability contract. This
        // extraction is controller-local and does not invoke the runner.
        super::super::snapshot::materialize_snapshot_piped_controlled(
            &local_path,
            &format!(
                "sh -c {}",
                shell::quote_arg(&format!(
                    "mkdir -p {source} && tar -p -C {source} -xf -",
                    source = shell::quote_arg(&source.display().to_string())
                ))
            ),
            &excludes,
            "prepare batch snapshot",
            Some(transfer.path()),
            control,
        )?;
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
        let synthetic = if options.mode == RunnerWorkspaceSyncMode::SnapshotGit && head.is_none() {
            let command = super::super::snapshot::synthetic_git_checkout_command(
                &source.display().to_string(),
                &snapshot,
                None,
                None,
                false,
            );
            control.shell(&command, "prepare synthetic batch Git baseline")?;
            let identity = super::super::snapshot::SyntheticCheckoutIdentity {
                synthetic_commit: control.git(&source, &["rev-parse", "HEAD"])?,
                synthetic_ref: control.git(&source, &["symbolic-ref", "HEAD"])?,
                synthetic_tree: control.git(&source, &["rev-parse", "HEAD^{tree}"])?,
            };
            fs::create_dir(directory.join("git")).map_err(io)?;
            fs::rename(source.join(".git"), directory.join("git/.git")).map_err(io)?;
            Some(identity)
        } else {
            None
        };
        let prepared_git = synthetic.is_some();
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
        || invoke(runner, transfer.path(), false, control),
    )?;
    let response: PlanResponse = serde_json::from_value(response).map_err(io)?;
    if response.identity != plan.identity {
        return Err(io(format!(
            "runner build identity mismatch: requested {}, received {}",
            plan.identity, response.identity
        )));
    }
    let mut requirement = SnapshotFilesystemRequirement {
        bytes: 0,
        inodes: 0,
    };
    for output in &outputs {
        let root_requirement = snapshot_filesystem_requirement(
            output.counts.bytes,
            output.content.as_ref().unwrap().0.entry_count as u64,
        );
        requirement.bytes = requirement.bytes.saturating_add(root_requirement.bytes);
        requirement.inodes = requirement.inodes.saturating_add(root_requirement.inodes);
    }
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
    homeboy_core::observation::phase_timing::time(
        "stage.lab.source_materialization.commit",
        || {
            homeboy_core::observation::phase_timing::time(
                "stage.lab.source_materialization.transfer_apply_verify",
                || invoke(runner, transfer.path(), true, control),
            )
        },
    )
    .map_err(|mut error| {
        error.details["workspace_effect"] = serde_json::json!({
            "runner_id": runner_id, "status": "commit_interrupted_or_unobserved",
            "roots": outputs.iter().map(|output| &output.remote_path).collect::<Vec<_>>(),
        });
        error
    })?;
    control.checkpoint()?;
    outputs.truncate(original_count);
    Ok(outputs)
}

pub(super) fn invoke(
    runner: &Runner,
    directory: &Path,
    commit: bool,
    control: &WorkspaceControl,
) -> Result<serde_json::Value> {
    control.checkpoint()?;
    #[cfg(test)]
    INVOCATIONS.with(|counter| counter.set(counter.get() + 1));
    let local_transport = runner.kind == RunnerKind::Local
        || super::super::util::ssh_client_for_runner(runner)
            .is_ok_and(|(_, client)| client.is_local);
    if local_transport {
        let response = if commit {
            materialization_commit_controlled(directory, control)
        } else {
            materialization_plan_controlled(&directory.join("plan.json"), control)
        }?;
        control.checkpoint()?;
        return Ok(response);
    }
    let binary = crate::remote_runner_homeboy_path(runner, "materialize Lab sources")?;
    let root = runner
        .workspace_root
        .as_deref()
        .ok_or_else(|| io("runner has no workspace root"))?;
    let remote = format!(
        "set -eu; mkdir -p {root}; stage=$(mktemp -d {template}); trap 'rm -rf \"$stage\"' EXIT HUP INT TERM; tar -p -C \"$stage\" -xf -; {binary} runner workspace materialization --input \"$stage{input}\" --response \"$stage/response.json\" {commit} >/dev/null; cat \"$stage/response.json\"",
        root = shell::quote_arg(root), template = shell::quote_arg(&format!("{root}/.materialization.XXXXXXXX")),
        binary = shell::quote_arg(binary), input = if commit { "" } else { "/plan.json" }, commit = if commit { "--commit" } else { "" },
    );
    let target = super::super::util::shell_command_for_runner(runner, &remote)?;
    let command = format!(
        "tar -C {} -cf - {} | {}",
        shell::quote_arg(&directory.display().to_string()),
        if commit { "." } else { "plan.json" },
        target
    );
    let output = control
        .output_exact(
            Command::new("bash").args(["-o", "pipefail", "-c", &command]),
            "runner materialization call",
        )
        .map_err(|mut error| {
            if commit && error.details["workspace_sync"]["command_started"] == true {
                error.details["workspace_sync"]["remote_effect_uncertain"] =
                    serde_json::json!(true);
            }
            error
        })?;
    if !output.status.success() {
        return super::super::util::shell_output_failure(output, "runner materialization call")
            .map(|()| serde_json::Value::Null).map_err(|mut error| {
                if commit && error.code == ErrorCode::RunnerLabTransportFailure {
                    error.details["workspace_sync"] = serde_json::json!({"command_started": true, "remote_effect_uncertain": true});
                }
                error
            });
    }
    control.checkpoint()?;
    json(&output.stdout)
}

#[cfg(test)]
thread_local! { pub(crate) static INVOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
