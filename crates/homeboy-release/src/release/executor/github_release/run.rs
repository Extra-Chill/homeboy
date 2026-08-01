//! The `github.release` step entry point that drives the full release lifecycle.

use crate::release::types::{ReleaseState, ReleaseStepResult};
use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};

use super::super::step_success;
use super::delivery::{existing_release_action, ExistingReleaseAction};
use super::gh_cli::manifest_declared_asset_names;
use super::gh_cli::{
    gh_command, gh_is_authenticated, gh_is_available, gh_release_exists,
    github_release_publications,
};
use super::notes::{
    build_github_release_body, github_changelog_url, github_release_notes_start_tag,
    persist_release_body,
};
use super::repair::{gh_auth_failure_message, github_release_repair_commands, log_repair_commands};
use super::results::{
    create_failed_result, not_created_result, published_existing_draft_result,
    published_release_url, skipped_result, unfinished_release_result, upload_failed_result,
    upload_success_result_with_publications,
};
use super::{
    download_small_release_asset, gh_failure_diagnostic, gh_release_metadata,
    github_release_upload_timeout, reconcile_release_publications, run_gh_command,
    validate_draft_adoption, verify_release_publications,
};

/// Create a GitHub Release for the just-pushed tag.
///
/// The step result must faithfully represent whether a GitHub Release object
/// now exists, because downstream `publish.<target>` / upload steps assume the
/// release is present (see issue #3541). The rules are:
///
/// - Release object created (or already exists) → `Success`.
/// - Release object NOT created and not recoverable here (no `gh` binary, not
///   authenticated, `gh release create` failed) → `Failed`, carrying the exact
///   recovery commands so the operator can resume from the pushed tag + built
///   artifacts without making a second tag.
/// - Generated release notes failed → we retry the create with fallback notes
///   (the changelog section, or a minimal body) and verify the release exists.
///   Only if that fallback create also fails do we mark the step `Failed`.
///
/// `github.release` is a release-pipeline show-stopper, so a `Failed` result
/// halts the plan before publish/upload runs against a non-existent release.
pub(crate) fn run_github_release(
    component: &Component,
    state: &ReleaseState,
    external_artifacts: bool,
) -> Result<ReleaseStepResult> {
    let tag = state.tag.clone().ok_or_else(|| {
        Error::internal_unexpected(
            "github.release: tag state not set (git.tag must run first)".to_string(),
        )
    })?;
    validate_declared_build_artifact(component, state, external_artifacts)?;
    let local_path = &component.local_path;

    let remote_url = component
        .remote_url
        .clone()
        .or_else(|| {
            homeboy_core::git::release_download::detect_remote_url(std::path::Path::new(local_path))
        })
        .ok_or_else(|| {
            Error::internal_unexpected(
                "github.release: no remote_url configured and git remote get-url origin failed"
                    .to_string(),
            )
        })?;

    let github =
        homeboy_core::git::release_download::parse_github_url(&remote_url).ok_or_else(|| {
            Error::validation_invalid_argument(
                "github.release",
                format!("Remote URL '{}' is not a GitHub URL", remote_url),
                None,
                Some(vec![
                    "Use a GitHub or GitHub Enterprise remote for automatic GitHub Releases"
                        .to_string(),
                    "Use --no-github-release to skip this step".to_string(),
                ]),
            )
        })?;

    // Collect artifact paths from state. Populated by release.package
    // (or any other extension action that emits artifact metadata into
    // ReleaseState::artifacts). Passing these to `gh release create` or
    // `gh release upload --clobber` attaches them to the Release in a
    // single API call — keeping the github.release step responsible for
    // the full Release lifecycle (entry + assets) instead of requiring a
    // separate publish.<target> step.
    let publications = github_release_publications(state)
        .map_err(|error| Error::validation_invalid_argument("release assets", error, None, None))?;
    let artifact_paths = publications
        .iter()
        .map(|publication| publication.upload_spec())
        .collect::<Vec<_>>();
    let has_artifacts = !publications.is_empty();
    // Single repair-command builder for every failure path. The persisted
    // exact-body file only exists after `persist_release_body` runs below, so
    // early paths (gh missing / unauthenticated / upload) pass `None` and
    // regenerate notes, while the create path passes the persisted path so the
    // repair `--notes-file` reproduces the body byte-for-byte (issue #3508).
    let repair_commands = |notes_start_tag: Option<&str>, persisted_notes: Option<&str>| {
        github_release_repair_commands(
            &tag,
            &github,
            &component.github,
            &artifact_paths,
            notes_start_tag,
            persisted_notes,
        )
    };

    if !gh_is_available() {
        let repair = repair_commands(None, None);
        homeboy_core::log_status!(
            "release",
            "✗ `gh` CLI not found on PATH — GitHub Release was NOT created"
        );
        log_repair_commands(&repair);
        return Ok(not_created_result(
            &tag,
            &github,
            "gh-not-available",
            "`gh` CLI not found on PATH; GitHub Release was not created.",
            repair,
        ));
    }

    if !gh_is_authenticated(&github, &component.github) {
        let repair = repair_commands(None, None);
        let auth_error = gh_auth_failure_message(&github, &repair);
        homeboy_core::log_status!(
            "release",
            "✗ `gh` is not authenticated — GitHub Release was NOT created"
        );
        homeboy_core::log_status!("release", "Authenticate with `gh auth login`, then run:");
        log_repair_commands(&repair);
        return Ok(not_created_result(
            &tag,
            &github,
            "gh-not-authenticated",
            &auth_error,
            repair,
        ));
    }

    let repo_flag = format!("{}/{}", github.owner, github.repo);
    if gh_release_exists(&github, &component.github, &tag, &repo_flag) {
        // A pre-existing release is a retry boundary. Read its PUBLICATION
        // state before deciding anything (issue #10441). By the time this step
        // runs the tag is already durable on `origin` — `release.yml`'s
        // cargo-dist matrix checks the tag out and builds with
        // `dist build --tag=<tag>`, so the tag has to exist before any artifact
        // can — which means an unpublished Draft here is a pushed tag that
        // ships nothing. This step is the pipeline's last chance to close that
        // window, so "a release object exists" is not sufficient to report
        // success; it must also be published.
        let metadata = match gh_release_metadata(&github, &component.github, &tag, &repo_flag) {
            Ok(metadata) => metadata,
            Err(error) => {
                let repair = repair_commands(None, None);
                let detail = format!(
                    "GitHub Release {} exists for {} but its publication state could not be read: {}",
                    tag, repo_flag, error
                );
                homeboy_core::log_status!("release", "✗ {}", detail);
                log_repair_commands(&repair);
                return Ok(unfinished_release_result(
                    &tag,
                    &github,
                    "release-state-unreadable",
                    &detail,
                    repair,
                    &error.diagnostics,
                ));
            }
        };

        if metadata.tag_name != tag {
            return Err(Error::validation_invalid_argument(
                "github.release",
                format!(
                    "GitHub Release tag '{}' does not match active tag '{}'",
                    metadata.tag_name, tag
                ),
                None,
                None,
            ));
        }
        if let Some(adoption) = state.draft_adoption.as_ref() {
            let sidecars = metadata
                .assets
                .iter()
                .filter(|asset| asset.name.ends_with(".sha256") || asset.name == "sha256.sum")
                .map(|asset| {
                    download_small_release_asset(&github, &component.github, &repo_flag, asset)
                        .map(|contents| (asset.name.clone(), contents))
                })
                .collect::<std::result::Result<std::collections::BTreeMap<_, _>, _>>()
                .map_err(|error| error.into_structured_error("release assets"))?;
            validate_draft_adoption(&tag, &adoption.expected_assets, &metadata, &sidecars)
                .map_err(|error| {
                    Error::validation_invalid_argument("release assets", error, None, None)
                })?;
            // Re-read immediately before un-drafting so a concurrent asset edit cannot race validation.
            let current = match gh_release_metadata(&github, &component.github, &tag, &repo_flag) {
                Ok(metadata) => metadata,
                Err(error) => {
                    let repair = repair_commands(None, None);
                    let detail = format!(
                        "GitHub Release {} exists for {} but its publication state could not be re-read before publishing: {}",
                        tag, repo_flag, error
                    );
                    homeboy_core::log_status!("release", "✗ {}", detail);
                    log_repair_commands(&repair);
                    return Ok(unfinished_release_result(
                        &tag,
                        &github,
                        "release-state-reread-failed",
                        &detail,
                        repair,
                        &error.diagnostics,
                    ));
                }
            };
            let current_sidecars = current
                .assets
                .iter()
                .filter(|asset| asset.name.ends_with(".sha256") || asset.name == "sha256.sum")
                .map(|asset| {
                    download_small_release_asset(&github, &component.github, &repo_flag, asset)
                        .map(|contents| (asset.name.clone(), contents))
                })
                .collect::<std::result::Result<std::collections::BTreeMap<_, _>, _>>()
                .map_err(|error| error.into_structured_error("release assets"))?;
            validate_draft_adoption(&tag, &adoption.expected_assets, &current, &current_sidecars)
                .map_err(|error| {
                Error::validation_invalid_argument("release assets", error, None, None)
            })?;
            let output = run_gh_command(
                gh_command(
                    &github,
                    &component.github,
                    &["release", "edit", &tag, "--draft=false", "-R", &repo_flag],
                ),
                github_release_upload_timeout(),
            );
            if output.timed_out || output.exit_code != Some(0) {
                let repair = repair_commands(None, None);
                let diagnostic = gh_failure_diagnostic(
                    "gh release edit --draft=false",
                    &format!("repos/{repo_flag}/releases/{tag}"),
                    &output,
                );
                let detail = diagnostic.summary.clone();
                homeboy_core::log_status!("release", "✗ {}", diagnostic.summary);
                log_repair_commands(&repair);
                return Ok(unfinished_release_result(
                    &tag,
                    &github,
                    "draft-adoption-publish-failed",
                    &detail,
                    repair,
                    &[diagnostic],
                ));
            }
            return Ok(published_existing_draft_result(
                &tag,
                &github,
                current.assets.len(),
                &published_release_url(&github, &tag, "", &output.stdout),
            ));
        }

        // Resolve the completeness contract the release declares for itself.
        // A draft's own `dist-manifest.json` names every archive it is supposed
        // to carry, so a partially uploaded draft can be recognised without any
        // out-of-band plan. An adoption manifest, when present, is authoritative
        // over it (#8687).
        let existing_asset_names = metadata
            .assets
            .iter()
            .map(|asset| asset.name.clone())
            .collect::<Vec<_>>();
        let declared_assets = state
            .draft_adoption
            .as_ref()
            .map(|adoption| adoption.expected_assets.clone())
            .or_else(|| {
                let manifest = metadata
                    .assets
                    .iter()
                    .find(|asset| asset.name == "dist-manifest.json")?;
                let contents =
                    download_small_release_asset(&github, &component.github, &repo_flag, manifest)
                        .ok()?;
                manifest_declared_asset_names(&contents)
            });

        match existing_release_action(
            metadata.is_draft,
            has_artifacts,
            &existing_asset_names,
            declared_assets.as_deref(),
            component.build_artifact.is_some(),
        ) {
            ExistingReleaseAction::AlreadyPublished => {
                homeboy_core::log_status!(
                    "release",
                    "GitHub Release {} is already published for {} — skipping (idempotent)",
                    tag,
                    repo_flag
                );
                return Ok(skipped_result(
                    &tag,
                    &github,
                    "release-already-published",
                    None,
                ));
            }
            ExistingReleaseAction::EmptyDraft => {
                // The component declares a downloadable artifact, so publishing
                // its empty draft would make an incomplete release `latest`.
                let repair = repair_commands(None, None);
                let detail = format!(
                    "GitHub Release {} for {} is an unpublished draft with no assets, and this run has no artifacts to attach. Refusing to publish an empty release over the pushed tag.",
                    tag, repo_flag
                );
                homeboy_core::log_status!("release", "✗ {}", detail);
                log_repair_commands(&repair);
                return Ok(unfinished_release_result(
                    &tag,
                    &github,
                    "draft-release-has-no-assets",
                    &detail,
                    repair,
                    &[],
                ));
            }
            ExistingReleaseAction::PartialDraft { missing } => {
                let detail = format!(
                    "GitHub Release {tag} for {repo_flag} is an unpublished draft missing {} asset(s) declared by its own distribution manifest: {}. Publishing it would ship a release whose missing platforms return 404.",
                    missing.len(),
                    missing.join(", ")
                );
                homeboy_core::log_status!("release", "{}", detail);
                let repair = repair_commands(None, None);
                return Ok(unfinished_release_result(
                    &tag,
                    &github,
                    "draft-release-incomplete",
                    &detail,
                    repair,
                    &[],
                ));
            }
            ExistingReleaseAction::PublishDraft => {
                homeboy_core::log_status!(
                    "release",
                    "GitHub Release {} for {} is an unpublished draft with {} asset(s) — publishing it so the pushed tag delivers",
                    tag,
                    repo_flag,
                    metadata.assets.len()
                );
                let publish_args = ["release", "edit", &tag, "--draft=false", "-R", &repo_flag];
                let publish_output = run_gh_command(
                    gh_command(&github, &component.github, &publish_args),
                    github_release_upload_timeout(),
                );
                if publish_output.timed_out || publish_output.exit_code != Some(0) {
                    let repair = repair_commands(None, None);
                    let diagnostic = gh_failure_diagnostic(
                        "gh release edit --draft=false",
                        &format!("repos/{repo_flag}/releases/{tag}"),
                        &publish_output,
                    );
                    let detail = diagnostic.summary.clone();
                    homeboy_core::log_status!("release", "✗ {}", diagnostic.summary);
                    log_repair_commands(&repair);
                    return Ok(unfinished_release_result(
                        &tag,
                        &github,
                        "draft-publish-failed",
                        &detail,
                        repair,
                        &[diagnostic],
                    ));
                }
                let url = published_release_url(&github, &tag, "", &publish_output.stdout);
                homeboy_core::log_status!(
                    "release",
                    "Published previously stranded GitHub Release: {}",
                    url
                );
                return Ok(published_existing_draft_result(
                    &tag,
                    &github,
                    metadata.assets.len(),
                    &url,
                ));
            }
            // This run carries artifacts: reconcile and verify the assets by
            // canonical name and digest before any publish decision is made.
            ExistingReleaseAction::ReconcileAssets => {}
        }

        let (uploads, existing) = reconcile_release_publications(
            &publications,
            &metadata.assets,
            &github,
            &component.github,
            &repo_flag,
        )
        .map_err(|error| error.into_structured_error("release assets"))?;
        homeboy_core::log_status!(
            "release",
            "GitHub Release {} already exists for {} — uploading {} canonical artifact(s), reusing {} verified artifact(s)",
            tag,
            repo_flag,
            uploads.len(),
            existing.len()
        );

        let upload_specs = uploads
            .iter()
            .map(|publication| publication.upload_spec())
            .collect::<Vec<_>>();
        let upload_output = if upload_specs.is_empty() {
            None
        } else {
            let mut upload_args: Vec<&str> = vec!["release", "upload", &tag];
            for path in &upload_specs {
                upload_args.push(path);
            }
            upload_args.extend_from_slice(&["-R", &repo_flag]);
            Some(run_gh_command(
                gh_command(&github, &component.github, &upload_args),
                github_release_upload_timeout(),
            ))
        };

        if upload_output
            .as_ref()
            .is_some_and(|output| output.timed_out || output.exit_code != Some(0))
        {
            let upload_output = upload_output.expect("checked upload output");
            let diagnostic = gh_failure_diagnostic(
                "gh release upload",
                &format!("repos/{repo_flag}/releases/{tag}/assets"),
                &upload_output,
            );
            let repair = repair_commands(None, None);
            homeboy_core::log_status!("release", "✗ {}", diagnostic.summary);
            log_repair_commands(&repair);
            return Ok(upload_failed_result(
                &tag,
                &github,
                upload_output.stdout,
                upload_output.stderr,
                upload_output.exit_code,
                upload_output.timed_out,
                artifact_paths.len(),
                repair,
                &[diagnostic],
            ));
        }

        let metadata = match gh_release_metadata(&github, &component.github, &tag, &repo_flag) {
            Ok(metadata) => metadata,
            Err(error) => {
                let diagnostics = error.diagnostics;
                return Ok(upload_failed_result(
                    &tag,
                    &github,
                    String::new(),
                    error.message,
                    None,
                    false,
                    artifact_paths.len(),
                    repair_commands(None, None),
                    &diagnostics,
                ));
            }
        };
        if let Err(error) = verify_release_publications(
            &publications,
            &metadata.assets,
            &github,
            &component.github,
            &repo_flag,
        ) {
            let diagnostics = error.diagnostics;
            return Ok(upload_failed_result(
                &tag,
                &github,
                String::new(),
                error.message,
                None,
                false,
                artifact_paths.len(),
                repair_commands(None, None),
                &diagnostics,
            ));
        }
        if metadata.is_draft {
            let publish_args = ["release", "edit", &tag, "--draft=false", "-R", &repo_flag];
            let publish_output = run_gh_command(
                gh_command(&github, &component.github, &publish_args),
                github_release_upload_timeout(),
            );
            if publish_output.timed_out || publish_output.exit_code != Some(0) {
                let diagnostic = gh_failure_diagnostic(
                    "gh release edit --draft=false",
                    &format!("repos/{repo_flag}/releases/{tag}"),
                    &publish_output,
                );
                return Ok(upload_failed_result(
                    &tag,
                    &github,
                    publish_output.stdout,
                    publish_output.stderr,
                    publish_output.exit_code,
                    publish_output.timed_out,
                    artifact_paths.len(),
                    repair_commands(None, None),
                    &[diagnostic],
                ));
            }
        }

        return Ok(upload_success_result_with_publications(
            &tag,
            &github,
            publications.len(),
            &publications,
        ));
    }

    let notes_start_tag = github_release_notes_start_tag(component, &tag);
    let changelog_url = github_changelog_url(component, &github, &tag);

    // Build the EXACT body Homeboy will post (issue #3508). This is the single
    // source of truth for the release body — generated notes + changelog footer,
    // or the changelog-section fallback + footer. Persisting it (below) lets the
    // repair commands reproduce the identical body via `--notes-file` instead of
    // re-deriving it from source and risking a divergent body.
    let body = build_github_release_body(
        component,
        &github,
        &tag,
        state,
        changelog_url.as_deref(),
        notes_start_tag.as_deref(),
    );
    let generated_notes_ok = body.generated_notes_ok;
    let release_notes = body.body.clone();

    // Persist the exact body so it is inspectable after the fact and so the
    // repair `--notes-file` reproduces it byte-for-byte. A failure to write the
    // artifact is non-fatal: fall back to commands that regenerate notes.
    let persisted_notes_path = persist_release_body(component, &tag, &release_notes);

    homeboy_core::log_status!(
        "release",
        "Creating GitHub Release {} on {} with {} artifact(s)...",
        tag,
        repo_flag,
        artifact_paths.len()
    );

    // Create a draft first. A public release immediately becomes eligible for
    // GitHub's latest-download endpoint, so it must not be visible until its
    // manifest-declared assets have been read back and verified.
    // Build args dynamically so we can append artifact paths as positional
    // arguments — `gh release create <tag> [files...]` attaches each file
    // as a Release asset in the same API call.
    let mut create_args: Vec<&str> = vec![
        "release",
        "create",
        &tag,
        "--title",
        &tag,
        "--notes",
        &release_notes,
        "--draft",
        "-R",
        &repo_flag,
    ];
    // Only pass --notes-start-tag when generated notes succeeded. With explicit
    // fallback `--notes`, re-triggering the note generation that just failed
    // would be pointless and could fail the create for the same reason.
    if generated_notes_ok {
        if let Some(previous_tag) = notes_start_tag.as_deref() {
            create_args.extend_from_slice(&["--notes-start-tag", previous_tag]);
        }
    }
    for path in &artifact_paths {
        create_args.push(path);
    }

    let output = run_gh_command(
        gh_command(&github, &component.github, &create_args),
        github_release_upload_timeout(),
    );

    if output.timed_out || output.exit_code != Some(0) {
        let repair = repair_commands(notes_start_tag.as_deref(), persisted_notes_path.as_deref());
        // Distinguish the path that brought us here so operators (and tests)
        // can see whether the fallback-after-generated-notes-failure also
        // failed, versus a plain create failure with working notes.
        let reason = if generated_notes_ok {
            "gh-command-failed"
        } else {
            "generated-notes-failed"
        };
        let diagnostic = gh_failure_diagnostic(
            "gh release create",
            &format!("repos/{repo_flag}/releases"),
            &output,
        );
        homeboy_core::log_status!("release", "✗ {}", diagnostic.summary);
        log_repair_commands(&repair);
        return Ok(create_failed_result(
            &tag,
            &github,
            reason,
            &output,
            repair,
            &body,
            persisted_notes_path.as_deref(),
            &[diagnostic],
        ));
    }

    let metadata = match gh_release_metadata(&github, &component.github, &tag, &repo_flag) {
        Ok(metadata) => metadata,
        Err(error) => {
            let diagnostics = error.diagnostics;
            return Ok(upload_failed_result(
                &tag,
                &github,
                String::new(),
                error.message,
                None,
                false,
                artifact_paths.len(),
                repair_commands(notes_start_tag.as_deref(), persisted_notes_path.as_deref()),
                &diagnostics,
            ));
        }
    };
    if let Err(error) = verify_release_publications(
        &publications,
        &metadata.assets,
        &github,
        &component.github,
        &repo_flag,
    ) {
        let diagnostics = error.diagnostics;
        return Ok(upload_failed_result(
            &tag,
            &github,
            String::new(),
            error.message,
            None,
            false,
            artifact_paths.len(),
            repair_commands(notes_start_tag.as_deref(), persisted_notes_path.as_deref()),
            &diagnostics,
        ));
    }
    let publish_args = ["release", "edit", &tag, "--draft=false", "-R", &repo_flag];
    let publish_output = run_gh_command(
        gh_command(&github, &component.github, &publish_args),
        github_release_upload_timeout(),
    );
    if publish_output.timed_out || publish_output.exit_code != Some(0) {
        let diagnostic = gh_failure_diagnostic(
            "gh release edit --draft=false",
            &format!("repos/{repo_flag}/releases/{tag}"),
            &publish_output,
        );
        return Ok(upload_failed_result(
            &tag,
            &github,
            publish_output.stdout,
            publish_output.stderr,
            publish_output.exit_code,
            publish_output.timed_out,
            artifact_paths.len(),
            repair_commands(notes_start_tag.as_deref(), persisted_notes_path.as_deref()),
            &[diagnostic],
        ));
    }

    let url = published_release_url(&github, &tag, &output.stdout, &publish_output.stdout);
    homeboy_core::log_status!("release", "Published verified GitHub Release: {}", url);
    Ok(step_success(
        "github.release",
        "github.release",
        Some(serde_json::json!({
            "action": "github.release",
            "tag": tag,
            "owner": github.owner,
            "repo": github.repo,
            "url": url,
            "artifact_count": artifact_paths.len(),
            "asset_publications": publications,
            "generated_notes": generated_notes_ok,
            "published": true,
            "changelog_url": body.changelog_url,
            "notes_start_tag": notes_start_tag,
            "release_body": release_notes,
            "release_body_source": body.source_label(),
            "release_body_file": persisted_notes_path,
        })),
        Vec::new(),
    ))
}

/// Package-owned releases must include the component deploy artifact. Externally
/// supplied release assets have their own validated inventory contract.
pub(crate) fn validate_declared_build_artifact(
    component: &Component,
    state: &ReleaseState,
    external_artifacts: bool,
) -> Result<()> {
    if external_artifacts {
        return Ok(());
    }
    let Some(declared_build_artifact) = component
        .build_artifact
        .as_deref()
        .filter(|path| !path.trim().is_empty())
    else {
        return Ok(());
    };
    let expected_name = std::path::Path::new(declared_build_artifact)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "build_artifact",
                format!(
                    "Configured build_artifact '{}' has no filename",
                    declared_build_artifact
                ),
                Some(declared_build_artifact.to_string()),
                None,
            )
        })?;
    let is_collected = state.artifacts.iter().any(|artifact| {
        let has_declared_identity = std::path::Path::new(&artifact.path)
            .file_name()
            .and_then(|name| name.to_str())
            == Some(expected_name);
        let has_bytes = std::path::Path::new(&artifact.path).is_file()
            || artifact
                .durable_path
                .as_deref()
                .is_some_and(|path| std::path::Path::new(path).is_file());
        has_declared_identity && has_bytes
    });
    if is_collected {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "build_artifact",
        format!(
            "Configured build_artifact '{}' is absent from the GitHub Release assets",
            declared_build_artifact
        ),
        Some(declared_build_artifact.to_string()),
        Some(vec![
            "Run release.package again and include the configured build_artifact before publishing the GitHub Release.".to_string(),
        ]),
    ))
}
