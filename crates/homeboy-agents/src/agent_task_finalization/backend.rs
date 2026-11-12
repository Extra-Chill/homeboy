use super::{
    AgentTaskPrDurableGateProof, AgentTaskPrFinalizationBackend, AgentTaskPrRef,
    AgentTaskPrResolvedBase, AgentTaskPublicationBinding, AgentTaskPublicationGitTracking,
};
use homeboy_core::error::{Error, Result};
use homeboy_core::git::{
    classify_git_push_failure, get_uncommitted_changes, pr_create_with_token, pr_edit_with_token,
    pr_find_with_token, pr_view_with_token, push_in_checkout,
    remote_branch_head_over_push_transport, resolve_effective_push_url, run_git, GitPushRemote,
    GitPushTransportClass, GitPushTransportKind, PrCreateOptions, PrEditOptions, PrFindOptions,
    PrState, PushOptions, DEFAULT_NON_INTERACTIVE_PUSH_TIMEOUT,
};
use homeboy_core::run_lifecycle_record::RunLifecycleRecord;
use std::path::Path;

fn publication_route(path: &str) -> Result<super::AgentTaskPublicationRoute> {
    let config = homeboy_core::defaults::load_config_for_read().map_err(|error| {
        publication_observation_context(error, "publication route: read config")
    })?;
    resolve_publication_route(path, &config.git_hosts)
}

fn resolve_publication_route(
    path: &str,
    hosts: &std::collections::HashMap<String, homeboy_core::defaults::GitHostConfig>,
) -> Result<super::AgentTaskPublicationRoute> {
    use homeboy_core::git::release_download::{detect_remote_url, parse_github_url};
    let base = detect_remote_url(Path::new(path)).and_then(|url| parse_github_url(&url));
    let mut route = super::AgentTaskPublicationRoute {
        route: "direct".into(),
        push_remote: "origin".into(),
        head_repository: base
            .as_ref()
            .map(|repo| format!("{}/{}", repo.owner, repo.repo))
            .unwrap_or_default(),
    };
    let Some(base) = base else { return Ok(route) };
    let policy = hosts
        .iter()
        .find(|(host, _)| host.eq_ignore_ascii_case(&base.host))
        .map(|(_, policy)| policy);
    let Some(policy) = policy else {
        return Ok(route);
    };
    let Some(owners) = &policy.direct_owners else {
        return Ok(route);
    };
    if owners
        .iter()
        .any(|owner| owner.eq_ignore_ascii_case(&base.owner))
    {
        return Ok(route);
    }
    let fork_owner = policy.fork_owner.as_deref().filter(|owner| !owner.trim().is_empty()).ok_or_else(|| {
        Error::git_command_failed(format!(
            "origin is {}/{} on {}; direct_owners={owners:?} selects fork publication but fork_owner is missing; set git_hosts.{}.fork_owner to your fork owner before retrying finalization.", base.owner, base.repo, base.host, base.host
        ))
    })?;
    let mut matches = Vec::new();
    for remote in git_output(path, &["remote"])?.lines() {
        let url = git_output(
            path,
            &["config", "--get", &format!("remote.{remote}.pushurl")],
        )
        .or_else(|_| git_output(path, &["config", "--get", &format!("remote.{remote}.url")]))?;
        if let Some(repo) = parse_github_url(&url) {
            if repo.host.eq_ignore_ascii_case(&base.host)
                && repo.owner.eq_ignore_ascii_case(fork_owner)
                && repo.repo.eq_ignore_ascii_case(&base.repo)
            {
                matches.push(remote.to_string());
            }
        }
    }
    if matches.len() != 1 {
        let remediation = if matches.is_empty() {
            format!("run `gh repo fork {}/{} --clone=false` then `git remote add {fork_owner} https://{}/{fork_owner}/{}.git`", base.owner, base.repo, base.host, base.repo)
        } else {
            format!("remove duplicate fork remotes with `git remote remove <duplicate>` until exactly one remote points to {fork_owner}/{}", base.repo)
        };
        return Err(Error::git_command_failed(format!(
            "origin is {}/{} on {}; direct_owners={owners:?}, fork_owner={fork_owner}; expected one fork remote for {fork_owner}/{}, observed {} ({matches:?}); {remediation}, then retry finalization.", base.owner, base.repo, base.host, base.repo, matches.len()
        )));
    }
    // A base-owner App cannot push to a personal fork. Fork publication uses
    // the user's gh/Git credentials, never the base installation token.
    route.route = "fork".into();
    route.push_remote = matches.remove(0);
    route.head_repository = format!("{fork_owner}/{}", base.repo);
    Ok(route)
}

fn publication_credential(
    path: &str,
) -> Result<
    Option<(
        homeboy_core::git::release_download::GitHubRepo,
        homeboy_core::git::github_app::PublicationCredential,
    )>,
> {
    let config = homeboy_core::defaults::load_config_for_read().map_err(|error| {
        publication_observation_context(error, "publication credential: read config")
    })?;
    if config.github_app.is_none() || publication_route(path)?.route == "fork" {
        return Ok(None);
    }
    let remote = homeboy_core::git::release_download::detect_remote_url(Path::new(path))
        .and_then(|url| homeboy_core::git::release_download::parse_github_url(&url))
        .ok_or_else(|| Error::git_command_failed("Cannot resolve publication repository"))?;
    let credential = homeboy_core::git::github_app::mint(config.github_app.as_ref(), &remote)
        .map_err(|error| {
            publication_observation_context(error, "publication credential: mint GitHub App token")
        })?;
    Ok(credential.map(|credential| (remote, credential)))
}

pub struct RealAgentTaskPrFinalizationBackend;

impl AgentTaskPrFinalizationBackend for RealAgentTaskPrFinalizationBackend {
    fn publication_route(
        &mut self,
        path: &str,
    ) -> Result<Option<super::AgentTaskPublicationRoute>> {
        let route = publication_route(path)?;
        Ok((!route.head_repository.is_empty()).then_some(route))
    }
    fn publication_context(
        &mut self,
        run_id: &str,
        lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
    ) -> Result<(String, Option<String>)> {
        let config = homeboy_core::defaults::load_config_for_read()?;
        let identity = homeboy_core::git::github_app::identity(config.github_app.as_ref());
        let record = match lifecycle_store.read_record(run_id) {
            Ok(record) => record,
            Err(error) if error.message.contains("agent-task run record not found") => {
                return Ok((identity, None))
            }
            Err(error) => return Err(error),
        };
        let requester = record
            .metadata
            .pointer("/client_context/requester")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        lifecycle_store.record_metadata_value(
            run_id,
            "publication_identity",
            serde_json::json!(identity),
        )?;
        Ok((identity, requester))
    }

    fn hydrate_run_in_store(
        &mut self,
        lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
        run_id: &str,
    ) -> Result<RunLifecycleRecord> {
        Ok(lifecycle_store.read_record(run_id)?.lifecycle)
    }

    fn hydrate_gate_proof_in_store(
        &mut self,
        lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
        run_id: &str,
    ) -> Result<AgentTaskPrDurableGateProof> {
        let run_id = lifecycle_store.read_record(run_id)?.run_id;
        let promotion = crate::agent_task_service::persisted_promotion_for_attempt_in_store(
            lifecycle_store,
            &run_id,
        )?
        .ok_or_else(missing_promotion)?;
        Ok(AgentTaskPrDurableGateProof { run_id, promotion })
    }

    fn hydrate_optional_gate_proof_in_store(
        &mut self,
        lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
        run_id: &str,
    ) -> Result<Option<AgentTaskPrDurableGateProof>> {
        optional_gate_proof(lifecycle_store.read_record(run_id)?)
    }

    fn current_branch(&mut self, path: &str) -> Result<String> {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(path)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !output.status.success() {
            return Err(Error::git_command_failed(format!(
                "git rev-parse failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn resolve_base(&mut self, path: &str, base: &str) -> Result<AgentTaskPrResolvedBase> {
        let reference = format!("refs/homeboy/finalization/base/{base}");
        let output = homeboy_core::git::with_remote_tracking_authority_until(
            Path::new(path),
            "fetch requested finalization base",
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            |remaining| {
                homeboy_core::git::run_git_output_with_env_timeout(
                    Path::new(path),
                    &[
                        "fetch",
                        "--no-tags",
                        "origin",
                        &format!("refs/heads/{base}:{reference}"),
                    ],
                    "fetch requested finalization base",
                    &[],
                    remaining,
                )
            },
        )?;
        if !output.status.success() {
            return Err(Error::validation_invalid_argument(
                "base",
                format!(
                    "could not fetch requested base `refs/heads/{base}` from origin; verify remote access and that the branch exists, then retry: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                None,
                None,
            ));
        }
        let sha = git_output(
            path,
            &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
        )?;
        Ok(AgentTaskPrResolvedBase { reference, sha })
    }

    fn resolve_verified_base(
        &mut self,
        path: &str,
        verified_base_sha: &str,
    ) -> Result<AgentTaskPrResolvedBase> {
        if !is_git_object_id(verified_base_sha) {
            return Err(Error::validation_invalid_argument(
                "verified_base_sha",
                "verified base snapshot must be a full Git object ID",
                Some(verified_base_sha.to_string()),
                None,
            ));
        }
        let sha = git_output(
            path,
            &[
                "rev-parse",
                "--verify",
                &format!("{verified_base_sha}^{{commit}}"),
            ],
        )
        .or_else(|_| {
            let fetch = homeboy_core::git::with_remote_tracking_authority_until(
                Path::new(path),
                "materialize verified finalization base",
                std::time::Instant::now() + std::time::Duration::from_secs(30),
                |remaining| homeboy_core::git::run_git_output_with_env_timeout(
                    Path::new(path),
                    &[
                        "fetch",
                        "--no-tags",
                        "--no-write-fetch-head",
                        "origin",
                        verified_base_sha,
                    ],
                    "materialize verified finalization base",
                    &[],
                    remaining,
                ),
            )?;
            if !fetch.status.success() {
                return Err(Error::validation_invalid_argument(
                    "verified_base_sha",
                    format!(
                        "verified base snapshot is unavailable locally and origin could not materialize exact commit `{verified_base_sha}`; retry from a worktree with origin access: {}",
                        String::from_utf8_lossy(&fetch.stderr).trim()
                    ),
                    Some(verified_base_sha.to_string()),
                    None,
                ));
            }
            git_output(
                path,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("{verified_base_sha}^{{commit}}"),
                ],
            )
            .map_err(|_| {
                Error::validation_invalid_argument(
                    "verified_base_sha",
                    "origin did not materialize the persisted verified base snapshot as a commit",
                    Some(verified_base_sha.to_string()),
                    None,
                )
            })
        })?;
        if sha != verified_base_sha {
            return Err(Error::validation_invalid_argument(
                "verified_base_sha",
                "verified base snapshot did not resolve to the supplied immutable Git object ID",
                Some(verified_base_sha.to_string()),
                None,
            ));
        }
        Ok(AgentTaskPrResolvedBase {
            reference: verified_base_sha.to_string(),
            sha,
        })
    }

    fn publication_base_sha(&mut self, path: &str, base: &str) -> Result<Option<String>> {
        let output = std::process::Command::new("git")
            .args([
                "ls-remote",
                "--heads",
                "origin",
                &format!("refs/heads/{base}"),
            ])
            .current_dir(path)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !output.status.success() {
            return Err(Error::git_command_failed(format!(
                "could not observe live origin base `{base}` before publication: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .map(str::to_string))
    }

    fn validate_publication_identity(
        &mut self,
        path: &str,
    ) -> Result<homeboy_core::git::GitIdentityProof> {
        homeboy_core::git::validate_publication_identity(path)
    }

    fn prepare_manual_candidate(
        &mut self,
        path: &str,
        base: &str,
    ) -> Result<(crate::agent_task::AgentTaskCandidate, Vec<String>)> {
        let head = git_output(path, &["rev-parse", "HEAD"])?;
        let changes = get_uncommitted_changes(path)?;
        let tree_sha = if changes.staged.is_empty()
            && changes.unstaged.is_empty()
            && changes.untracked.is_empty()
        {
            git_output(path, &["rev-parse", "HEAD^{tree}"])?
        } else {
            git_output(path, &["add", "-A"])?;
            git_output(path, &["write-tree"])?
        };
        // A staged merge is a merge commit, not a flat tree on HEAD: its
        // parents are what keep the published branch from re-carrying what
        // it merged (#17346).
        let merged = homeboy_core::git::pending_merge(Path::new(path))?;
        let candidate_sha = if merged.is_empty() {
            head
        } else {
            let mut args = vec![
                "-c",
                "user.name=Homeboy",
                "-c",
                "user.email=homeboy@localhost",
                "commit-tree",
                tree_sha.as_str(),
                "-p",
                head.as_str(),
            ];
            for parent in &merged {
                args.extend(["-p", parent.as_str()]);
            }
            args.extend(["-m", "Staged Cook candidate"]);
            git_output(path, &args)?
        };
        let files = git_output(path, &["diff", "--name-only", base, &tree_sha])?
            .lines()
            .map(str::to_string)
            .collect();
        Ok((
            crate::agent_task::AgentTaskCandidate {
                base_sha: base.into(),
                candidate_sha,
                tree_sha,
            },
            files,
        ))
    }

    fn candidate_dates(&mut self, path: &str, candidate_sha: &str) -> Result<(String, String)> {
        let dates = git_output(path, &["show", "-s", "--format=%aI%n%cI", candidate_sha])?;
        let mut lines = dates.lines();
        Ok((
            lines.next().unwrap_or_default().into(),
            lines.next().unwrap_or_default().into(),
        ))
    }

    fn publish_tree(
        &mut self,
        path: &str,
        candidate: &crate::agent_task::AgentTaskCandidate,
        parent: &str,
        message: &str,
        dates: &(String, String),
    ) -> Result<String> {
        let merged =
            homeboy_core::git::merged_parents(Path::new(path), parent, &candidate.candidate_sha)?;
        let mut args = vec!["commit-tree", candidate.tree_sha.as_str(), "-p", parent];
        for merged in &merged {
            args.extend(["-p", merged.as_str()]);
        }
        args.extend(["-m", message]);
        let output = std::process::Command::new("git")
            .current_dir(path)
            .args(&args)
            .env("GIT_AUTHOR_DATE", &dates.0)
            .env("GIT_COMMITTER_DATE", &dates.1)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !output.status.success() {
            return Err(Error::git_command_failed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn push_branch(
        &mut self,
        path: &str,
        commit_sha: &str,
        head: &str,
    ) -> Result<AgentTaskPublicationGitTracking> {
        // Resolve the transport the push will actually use (pushurl beats a
        // pushInsteadOf rewrite) before mutating anything, so a classified
        // failure can report it (#10734).
        let route = publication_route(path)?;
        let push_remote = route.push_remote.clone();
        let effective_remote = resolve_effective_push_url(Path::new(path), &push_remote).ok();
        let credential = publication_credential(path)?;
        let expected = remote_branch_head(path, head)?.unwrap_or_default();
        let output = push_in_checkout(
            "",
            path,
            PushOptions {
                remote_url: Some(
                    credential
                        .as_ref()
                        .map(|(repo, _)| {
                            format!("https://{}/{}/{}.git", repo.host, repo.owner, repo.repo)
                        })
                        .unwrap_or_else(|| push_remote.clone()),
                ),
                token: credential
                    .as_ref()
                    .map(|(_, credential)| credential.token().to_string()),
                strip_extraheader: credential.is_some(),
                refspec: Some(format!("{commit_sha}:refs/heads/{head}")),
                lease: Some(format!("refs/heads/{head}:{expected}")),
                non_interactive: true,
                timeout: Some(DEFAULT_NON_INTERACTIVE_PUSH_TIMEOUT),
                ..Default::default()
            },
        )?;
        if !output.success {
            let classification = classify_git_push_failure(&output.stderr);
            if classification != GitPushTransportClass::Other {
                return Err(blocked_publication_push_error(
                    effective_remote.as_ref().unwrap_or(&GitPushRemote {
                        remote: push_remote.clone(),
                        effective_push_url: String::new(),
                        transport_kind: GitPushTransportKind::Unknown,
                        host: None,
                    }),
                    classification,
                    &output.stderr,
                    PushPreflightPhase::Push,
                ));
            }
            return Err(Error::git_command_failed(format!(
                "git push failed: {}",
                output.stderr
            )));
        }
        let verified_remote_sha = remote_branch_head(path, head)?.ok_or_else(|| {
            Error::git_command_failed(format!("pushed origin head `{head}` could not be verified"))
        })?;
        if verified_remote_sha != commit_sha {
            return Err(Error::git_command_failed(format!(
                "pushed origin head `{head}` resolved to `{verified_remote_sha}` instead of `{commit_sha}`"
            )));
        }

        git_output(
            path,
            &["update-ref", &format!("refs/heads/{head}"), commit_sha],
        )?;
        // HEAD is now the published merge; conclude the pending one.
        homeboy_core::git::record_pending_merge(Path::new(path), &[])?;
        let upstream_ref = format!("refs/remotes/{push_remote}/{head}");
        let existing_upstream =
            git_output(path, &["rev-parse", "--symbolic-full-name", "@{upstream}"]).ok();
        git_output(path, &["update-ref", &upstream_ref, &verified_remote_sha])?;
        if existing_upstream.as_deref() != Some(upstream_ref.as_str()) {
            git_output(
                path,
                &[
                    "branch",
                    "--set-upstream-to",
                    &format!("{push_remote}/{head}"),
                    head,
                ],
            )?;
        }

        Ok(AgentTaskPublicationGitTracking {
            local_branch: head.to_string(),
            remote: push_remote,
            upstream_ref,
            verified_remote_sha,
        })
    }

    fn find_open_pr(
        &mut self,
        component_id: Option<&str>,
        path: &str,
        base: &str,
        head: &str,
    ) -> Result<Option<AgentTaskPrRef>> {
        let credential = publication_credential(path)?;
        let output = pr_find_with_token(
            component_id,
            PrFindOptions {
                base: Some(base.to_string()),
                head: Some(publication_route(path)?.pr_head(head)),
                state: PrState::Open,
                limit: 10,
                path: Some(path.to_string()),
            },
            credential
                .as_ref()
                .map(|(_, credential)| credential.token()),
        )?;
        Ok(output.items.into_iter().next().map(|item| AgentTaskPrRef {
            number: item.number,
            url: item.url,
            is_draft: item.is_draft,
        }))
    }

    fn find_merged_pr(
        &mut self,
        component_id: Option<&str>,
        path: &str,
        base: &str,
        head: &str,
    ) -> Result<Option<AgentTaskPrRef>> {
        let credential = publication_credential(path)?;
        let output = pr_find_with_token(
            component_id,
            PrFindOptions {
                base: Some(base.to_string()),
                head: Some(publication_route(path)?.pr_head(head)),
                state: PrState::Merged,
                limit: 10,
                path: Some(path.to_string()),
            },
            credential
                .as_ref()
                .map(|(_, credential)| credential.token()),
        )?;
        Ok(output.items.into_iter().next().map(|item| AgentTaskPrRef {
            number: item.number,
            url: item.url,
            is_draft: item.is_draft,
        }))
    }

    fn open_pr_title(
        &mut self,
        component_id: Option<&str>,
        path: &str,
        number: u64,
    ) -> Result<Option<String>> {
        let credential = publication_credential(path)?;
        Ok(pr_view_with_token(
            component_id,
            number,
            Some(path.to_string()),
            credential
                .as_ref()
                .map(|(_, credential)| credential.token()),
        )?
        .title)
    }

    fn open_pr_body(
        &mut self,
        component_id: Option<&str>,
        path: &str,
        number: u64,
    ) -> Result<Option<String>> {
        let credential = publication_credential(path)?;
        Ok(pr_view_with_token(
            component_id,
            number,
            Some(path.to_string()),
            credential
                .as_ref()
                .map(|(_, credential)| credential.token()),
        )?
        .body)
    }

    fn create_pr(
        &mut self,
        component_id: Option<&str>,
        path: &str,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<AgentTaskPrRef> {
        let credential = publication_credential(path)?;
        let output = pr_create_with_token(
            component_id,
            PrCreateOptions {
                base: base.to_string(),
                head: publication_route(path)?.pr_head(head),
                title: title.to_string(),
                body: body.to_string(),
                draft,
                path: Some(path.to_string()),
            },
            credential
                .as_ref()
                .map(|(_, credential)| credential.token()),
        )?;
        Ok(AgentTaskPrRef {
            number: output.number.unwrap_or_default(),
            url: output.url.unwrap_or_default(),
            is_draft: draft,
        })
    }

    fn update_pr(
        &mut self,
        component_id: Option<&str>,
        path: &str,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef> {
        let credential = publication_credential(path)?;
        let output = pr_edit_with_token(
            component_id,
            PrEditOptions {
                number,
                title: Some(title.to_string()),
                body: Some(body.to_string()),
                path: Some(path.to_string()),
            },
            credential
                .as_ref()
                .map(|(_, credential)| credential.token()),
        )?;
        Ok(AgentTaskPrRef {
            number,
            url: output.url.unwrap_or_default(),
            // Editing a PR's body and title never changes its review state.
            is_draft: false,
        })
    }

    fn mark_pr_ready(&mut self, path: &str, pr: &AgentTaskPrRef) -> Result<AgentTaskPrRef> {
        let output = publication_gh_output(path, &["pr", "ready", &pr.number.to_string()])?;
        if !output.status.success() {
            return Err(Error::git_command_failed(format!(
                "gh pr ready failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(AgentTaskPrRef {
            number: pr.number,
            url: pr.url.clone(),
            is_draft: false,
        })
    }

    fn verify_publication_binding(
        &mut self,
        path: &str,
        _base: &str,
        _head: &str,
        candidate_sha: &str,
        changed_files: &[String],
        pr: &AgentTaskPrRef,
    ) -> Result<AgentTaskPublicationBinding> {
        let candidate_tree =
            git_output(path, &["rev-parse", &format!("{candidate_sha}^{{tree}}")])?;
        let repository = gh_json(path, &["repo", "view", "--json", "nameWithOwner"])?
            ["nameWithOwner"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let pr_value = gh_json(
            path,
            &[
                "pr",
                "view",
                &pr.number.to_string(),
                "--json",
                "baseRefName,headRefName,headRefOid,headRepository,headRepositoryOwner",
            ],
        )?;
        let pr_head_sha = pr_value["headRefOid"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // Incomplete JSON is unavailable evidence, not an observed ref mismatch.
        if repository.is_empty()
            || pr_head_sha.is_empty()
            || pr_value["baseRefName"].as_str().is_none()
            || pr_value["headRefName"].as_str().is_none()
        {
            return Err(Error::git_command_failed(
                "binding: gh returned incomplete repository or PR identity",
            ));
        }
        let head_repository = pr_head_repository_from_json(&pr_value);
        if head_repository.is_empty() {
            return Err(Error::git_command_failed(
                "binding: gh returned incomplete PR head repository",
            ));
        }
        Ok(AgentTaskPublicationBinding {
            candidate_sha: candidate_sha.to_string(),
            candidate_tree,
            remote_sha: candidate_sha.to_string(),
            pr_head_sha,
            repository,
            head_repository,
            changed_files: super::normalize_changed_files(changed_files),
        })
    }
}

/// Finalization consumes the persisted promotion's content, so it reads it
/// through the resolving reader: large reports are stored as metadata
/// payload references, not inline (#16664).
fn optional_gate_proof(
    record: crate::agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<Option<AgentTaskPrDurableGateProof>> {
    let run_id = record.run_id.clone();
    Ok(
        crate::agent_task_service::persisted_promotion_from_record(&run_id, record)?
            .map(|promotion| AgentTaskPrDurableGateProof { run_id, promotion }),
    )
}

fn missing_promotion() -> Error {
    Error::validation_invalid_argument(
        "run_id",
        "normal finalization requires the run's persisted applied promotion; run agent-task promote first or use --manual-finalization",
        None,
        None,
    )
}

/// Read the live remote branch head through the effective push transport
/// (#10734): the destination the publication push targets (pushurl beats a
/// `pushInsteadOf` rewrite), contacted the same way the push contacts it —
/// configured host transport, `GIT_TERMINAL_PROMPT=0`, SSH `BatchMode=yes`
/// only without a caller `GIT_SSH_COMMAND`, bounded by the push deadline —
/// so a read that cannot reach the push destination classifies as the same
/// recoverable blocked operator action as the push instead of an
/// unclassified failure.
fn remote_branch_head(path: &str, head: &str) -> Result<Option<String>> {
    let git_root = Path::new(path);
    if let Some((repo, credential)) = publication_credential(path)? {
        let url = format!(
            "https://x-access-token:{}@{}/{}/{}.git",
            credential.token(),
            repo.host,
            repo.owner,
            repo.repo
        );
        let output = homeboy_core::git::run_git_output_with_env_timeout(
            git_root,
            &[
                "-c",
                "credential.helper=",
                "-c",
                "http.extraHeader=",
                "ls-remote",
                &url,
                &format!("refs/heads/{head}"),
            ],
            "read GitHub App publication head",
            &[("GIT_TERMINAL_PROMPT".into(), "0".into())],
            DEFAULT_NON_INTERACTIVE_PUSH_TIMEOUT,
        )
        .map_err(|_| Error::git_command_failed("GitHub App remote head read failed"))?;
        if !output.status.success() {
            return Err(Error::git_command_failed(
                "GitHub App remote head read failed",
            ));
        }
        return Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .map(str::to_owned));
    }
    let remote = publication_route(path)?.push_remote;
    let read = remote_branch_head_over_push_transport(git_root, &remote, head)?;
    if !read.success {
        let classification = classify_git_push_failure(&read.stderr);
        if classification != GitPushTransportClass::Other {
            return Err(blocked_publication_push_error(
                &read.resolved,
                classification,
                &read.stderr,
                PushPreflightPhase::RemoteHeadRead,
            ));
        }
        return Err(Error::git_command_failed(format!(
            "could not read live `{remote}` head `{head}` before publication: {}",
            read.stderr
        )));
    }
    Ok(read.head)
}

/// Publication step a classified transport failure blocked, distinguishing
/// the pre-publication remote-head read from the push itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushPreflightPhase {
    Push,
    RemoteHeadRead,
}

impl PushPreflightPhase {
    fn label(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::RemoteHeadRead => "remote_head_read",
        }
    }

    fn blocked_action(self) -> &'static str {
        match self {
            Self::Push => "publication push to",
            Self::RemoteHeadRead => "pre-publication read of the remote head from",
        }
    }
}

/// A push (or the pre-publication remote-head read routed over the same push
/// transport) that failed for a classified transport/authentication reason is
/// a recoverable blocked operator action, not a candidate failure: the commit
/// already exists locally, and retrying finalization after recovery resumes
/// the same verified candidate without a duplicate commit or PR (#10734).
fn blocked_publication_push_error(
    resolved: &GitPushRemote,
    classification: GitPushTransportClass,
    stderr: &str,
    phase: PushPreflightPhase,
) -> Error {
    let policy = homeboy_core::redaction::RedactionPolicy::default();
    let sanitized_stderr = policy.redact_embedded_urls(stderr).trim().to_string();
    let host = resolved.host.clone().unwrap_or_default();
    let recovery = push_recovery_hint(resolved, classification, &host);
    let mut error = Error::internal_unexpected(format!(
        "{} remote `{}` over {} transport is blocked by a recoverable operator action ({}): retrying finalization after recovery reuses the same verified candidate; {}",
        phase.blocked_action(),
        resolved.remote,
        resolved.transport_kind.label(),
        classification.label(),
        sanitized_stderr,
    ))
    .with_retryable(true);
    error.details["field"] = serde_json::json!("publication_push");
    error.details["git_push_preflight"] = serde_json::json!({
        "schema": "homeboy/git-push-preflight/v1",
        "phase": phase.label(),
        "remote": resolved.remote,
        "effective_push_url": resolved.effective_push_url,
        "transport_kind": resolved.transport_kind.label(),
        "host": resolved.host,
        "classification": classification.label(),
        "git_stderr": sanitized_stderr,
        "recovery": recovery,
    });
    error.hints.push(homeboy_error::Hint { message: recovery });
    error
}

/// Generic, host-agnostic recovery wording. Homeboy never rewrites the
/// remote, disables commit signing, or injects credentials; it names the
/// operator-owned options instead.
fn push_recovery_hint(
    resolved: &GitPushRemote,
    classification: GitPushTransportClass,
    host: &str,
) -> String {
    let retry = "then retry finalization; the same verified candidate is reused without creating a duplicate commit or PR";
    match classification {
        GitPushTransportClass::SshAgentRefused
            if resolved.transport_kind == GitPushTransportKind::Ssh && !host.is_empty() =>
        {
            format!(
                "The SSH agent refused to sign for `{host}` (a user-presence-gated agent refuses while the screen is locked). Unlock the host or authorize the agent, {retry}. Alternatively, keep pushes working by configuring github_hosts.{host}.env with `url.https://{host}/.pushInsteadOf` to route pushes over HTTPS, and remove a redundant remote.{}.pushurl if one is set (Git ignores pushInsteadOf when an explicit pushurl exists).",
                resolved.remote
            )
        }
        GitPushTransportClass::SshAgentRefused => format!(
            "The SSH agent refused to sign. Unlock the host or authorize the agent, {retry}."
        ),
        GitPushTransportClass::SshAgentUnavailable => format!(
            "The SSH agent is unavailable (no agent connection could be opened). Restore the agent socket, {retry}."
        ),
        GitPushTransportClass::AuthRejected => format!(
            "The push was rejected by authentication. Restore credential access (authorize the SSH key or refresh the HTTPS credential helper), {retry}."
        ),
        GitPushTransportClass::TransportUnreachable => format!(
            "The push remote is currently unreachable (network, DNS, or proxy; some proxies only reconnect after the session is unlocked). Restore reachability, {retry}."
        ),
        GitPushTransportClass::Other => unreachable!("caller filters the Other class"),
    }
}

/// Returns `true` when `remote_head` is contained in the finalization candidate
/// (`local_head`), i.e. the candidate is a fast-forward over the live remote and
/// Homeboy's push would not clobber any remote-only commits.
///
/// The remote commit may be absent from the local object database (an executor
/// pushed it directly), so fetch it best-effort first. If it still cannot be
/// resolved, the divergence cannot be proven safe — treat it as non-ancestor so
/// finalization fails closed with the out-of-contract diagnostic.

/// Run git and return trimmed stdout. Thin delegation to the canonical
/// `homeboy_core::git::run_git` so the command spelling and failure handling
/// live in one place (richer diagnostics: command, cwd, exit code, stderr).
/// `run_git` returns raw stdout, so trim here to preserve this wrapper's
/// long-standing trimmed contract for its call sites.
fn git_output(path: &str, args: &[&str]) -> Result<String> {
    run_git(
        std::path::Path::new(path),
        args,
        &format!("git {}", args.join(" ")),
    )
    .map(|stdout| stdout.trim().to_string())
    .map_err(|error| publication_observation_context(error, &format!("git {}", args.join(" "))))
}

/// Tags an observation failure with the step that produced it, so a bare
/// `IO error` still names where publication evidence became unavailable.
fn publication_observation_context(mut error: Error, stage: &str) -> Error {
    if error.details.is_null() {
        error.details = serde_json::json!({});
    }
    if let Some(details) = error.details.as_object_mut() {
        details.insert("publication_observation".into(), serde_json::json!(stage));
    }
    error
}

fn publication_gh_output(path: &str, args: &[&str]) -> Result<std::process::Output> {
    let credential = publication_credential(path)?;
    let mut command = std::process::Command::new("gh");
    command.args(args).current_dir(path);
    // Publication targets the checkout's origin. Without GH_REPO, gh resolves
    // a GitHub fork to its parent (or to a fork after `repo fork`), so PR
    // reads and writes would land on another repository's PR numbers.
    if let Some(repo) = homeboy_core::git::release_download::detect_remote_url(Path::new(path))
        .and_then(|url| homeboy_core::git::release_download::parse_github_url(&url))
    {
        command.env(
            "GH_REPO",
            format!("{}/{}/{}", repo.host, repo.owner, repo.repo),
        );
    }
    if let Some((repo, credential)) = &credential {
        command
            .env("GH_TOKEN", credential.token())
            .env("GH_ENTERPRISE_TOKEN", credential.token())
            .env("GH_HOST", &repo.host);
    }
    let mut output = command
        .output()
        .map_err(|_| Error::git_command_failed("Could not invoke gh"))?;
    if let Some((_, credential)) = credential {
        output.stdout = String::from_utf8_lossy(&output.stdout)
            .replace(credential.token(), "[REDACTED]")
            .into_bytes();
        output.stderr = String::from_utf8_lossy(&output.stderr)
            .replace(credential.token(), "[REDACTED]")
            .into_bytes();
    }
    Ok(output)
}

fn gh_json(path: &str, args: &[&str]) -> Result<serde_json::Value> {
    let output = publication_gh_output(path, args).map_err(|error| {
        publication_observation_context(error, &format!("gh {}", args.join(" ")))
    })?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "gh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        Error::git_command_failed(format!(
            "gh {} returned invalid JSON: {error}",
            args.join(" ")
        ))
    })
}

/// Derives a PR head repository's `owner/name` slug from a `gh pr view --json
/// headRepository,headRepositoryOwner` payload.
///
/// `gh` does not populate `headRepository.nameWithOwner`; that field is only
/// present on `repo view`. The owner login must instead be read from the
/// separate `headRepositoryOwner` field and joined with `headRepository.name`.
/// If either half is missing, this returns an empty string so the publication
/// binding check fails closed instead of silently treating an unresolved
/// owner as a same-repository match.
fn pr_head_repository_from_json(pr_value: &serde_json::Value) -> String {
    let owner = pr_value["headRepositoryOwner"]["login"].as_str();
    let name = pr_value["headRepository"]["name"].as_str();
    match (owner, name) {
        (Some(owner), Some(name)) if !owner.is_empty() && !name.is_empty() => {
            format!("{owner}/{name}")
        }
        _ => String::new(),
    }
}

fn is_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod pr_head_repository_tests {
    use super::pr_head_repository_from_json;
    use serde_json::json;

    #[test]
    fn resolves_owner_and_name_from_the_real_gh_payload() {
        // Verified against `gh pr view 911 -R Extra-Chill/extrachill-events
        // --json headRepository,headRepositoryOwner,isCrossRepository`.
        let value = json!({
            "headRepository": {"id": "R_kgDOP30Kgg", "name": "extrachill-events"},
            "headRepositoryOwner": {"id": "O_kgDODalOjg", "login": "Extra-Chill"},
            "isCrossRepository": false,
        });
        assert_eq!(
            pr_head_repository_from_json(&value),
            "Extra-Chill/extrachill-events"
        );
    }

    #[test]
    fn a_fork_head_carries_the_forks_owner_login() {
        let value = json!({
            "headRepository": {"id": "R_fork", "name": "extrachill-events"},
            "headRepositoryOwner": {"id": "O_fork", "login": "some-contributor"},
            "isCrossRepository": true,
        });
        // Same repository name, different owner: this must not collapse to
        // the base repository's `owner/name` slug.
        assert_eq!(
            pr_head_repository_from_json(&value),
            "some-contributor/extrachill-events"
        );
        assert_ne!(
            pr_head_repository_from_json(&value),
            "Extra-Chill/extrachill-events"
        );
    }

    #[test]
    fn missing_owner_fails_closed_to_an_empty_string() {
        let value = json!({
            "headRepository": {"id": "R_kgDOP30Kgg", "name": "extrachill-events"},
        });
        assert_eq!(pr_head_repository_from_json(&value), "");
    }

    #[test]
    fn missing_repository_name_also_fails_closed() {
        let value = json!({
            "headRepositoryOwner": {"id": "O_kgDODalOjg", "login": "Extra-Chill"},
        });
        assert_eq!(pr_head_repository_from_json(&value), "");
    }

    #[test]
    fn a_wholly_absent_payload_fails_closed() {
        assert_eq!(pr_head_repository_from_json(&json!({})), "");
    }
}

#[cfg(test)]
mod remote_base_tests {
    use super::*;
    use std::process::Command;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use homeboy_core::test_support::run_git_command as git;

    /// `GIT_SSH_COMMAND` is process-global, so tests that set it must not
    /// overlap. Rewrites go in repo-local config, never `GIT_CONFIG_*` env.
    static PUSH_TRANSPORT_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn push_transport_env_lock() -> MutexGuard<'static, ()> {
        PUSH_TRANSPORT_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("temp repo");
        git(repo.path(), &["init", "-b", "main"]);
        git(repo.path(), &["config", "user.email", "test@example.com"]);
        git(repo.path(), &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("old.txt"), "old").expect("base file");
        std::fs::write(repo.path().join("removed.txt"), "removed").expect("base file");
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "base"]);
        repo
    }

    fn base(reference: &str, sha: &str) -> AgentTaskPrResolvedBase {
        AgentTaskPrResolvedBase {
            reference: reference.to_string(),
            sha: sha.to_string(),
        }
    }

    fn publication_repo() -> (tempfile::TempDir, tempfile::TempDir) {
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "--bare", "-b", "main"]);
        let repo = repo();
        git(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-u", "origin", "main"]);
        git(repo.path(), &["checkout", "-b", "feature"]);
        std::fs::write(repo.path().join("feature.txt"), "feature").unwrap();
        git(repo.path(), &["add", "feature.txt"]);
        git(repo.path(), &["commit", "-m", "feature"]);
        (repo, origin)
    }

    fn upstream(path: &std::path::Path) -> Result<String> {
        git_output(
            path.to_str().unwrap(),
            &["rev-parse", "--symbolic-full-name", "@{upstream}"],
        )
    }

    #[test]
    fn owner_policy_routes_real_publication_and_persists_coordinates() {
        homeboy_core::test_support::with_isolated_home(|_| {
            for (owner, expected_route, expected_remote, expected_head) in [
                ("eXtRa-ChIlL", "direct", "origin", "feature"),
                ("WordPress", "fork", "chubes4", "chubes4:feature"),
            ] {
                let (repo, origin) = publication_repo();
                let fork = tempfile::tempdir().unwrap();
                git(fork.path(), &["init", "--bare", "-b", "main"]);
                let base_url = format!("https://github.com/{owner}/reprint.git");
                let fork_url = "https://github.com/chubes4/reprint.git";
                git(repo.path(), &["remote", "set-url", "origin", &base_url]);
                git(repo.path(), &["remote", "add", "chubes4", fork_url]);
                for (url, destination) in [(&*base_url, origin.path()), (fork_url, fork.path())] {
                    git(
                        repo.path(),
                        &[
                            "config",
                            &format!("url.{}.pushInsteadOf", destination.display()),
                            url,
                        ],
                    );
                }
                homeboy_core::defaults::save_config(&homeboy_core::defaults::HomeboyConfig {
                    git_hosts: std::collections::HashMap::from([(
                        "github.com".into(),
                        homeboy_core::defaults::GitHostConfig {
                            direct_owners: Some(vec!["Extra-Chill".into()]),
                            fork_owner: Some("chubes4".into()),
                            ..Default::default()
                        },
                    )]),
                    ..Default::default()
                })
                .unwrap();
                let path = repo.path().to_str().unwrap();
                let route = publication_route(path).unwrap();
                assert_eq!(route.route, expected_route);
                assert_eq!(route.push_remote, expected_remote);
                assert_eq!(route.pr_head("feature"), expected_head);
                let sha = git_output(path, &["rev-parse", "HEAD"]).unwrap();
                let tracking = RealAgentTaskPrFinalizationBackend
                    .push_branch(path, &sha, "feature")
                    .unwrap();
                assert_eq!(tracking.remote, expected_remote);
                assert_eq!(
                    remote_branch_head(path, "feature").unwrap().as_deref(),
                    Some(sha.as_str())
                );
                assert_eq!(
                    upstream(repo.path()).unwrap(),
                    format!("refs/remotes/{expected_remote}/feature")
                );
                let untouched = if expected_route == "fork" {
                    origin.path()
                } else {
                    fork.path()
                };
                assert!(git_output(
                    untouched.to_str().unwrap(),
                    &["show-ref", "--verify", "refs/heads/feature"]
                )
                .is_err());
                let mut intent = super::super::build_pr_publication_intent(
                    &super::super::tests::options(),
                    "feature",
                    &[],
                    super::super::build_finalization_proof(&super::super::tests::options(), vec![]),
                    &base("main", &sha),
                );
                intent.target.publication_route = Some(route.clone());
                let proof = super::super::publication_proof(
                    &intent,
                    "published",
                    "create",
                    None,
                    None,
                    Some(tracking),
                    None,
                );
                let value = serde_json::to_value(proof).unwrap();
                assert_eq!(
                    value["target"]["publication_route"]["route"],
                    expected_route
                );
                assert_eq!(
                    value["target"]["publication_route"]["head_repository"],
                    route.head_repository
                );
            }
        });
    }

    /// A checkout of a GitHub fork publishes directly to that fork. gh must
    /// target the origin there too: unpinned, it resolves the fork's parent,
    /// so PR numbers and quarantine writes land on someone else's PRs.
    #[test]
    fn every_publication_gh_call_targets_the_checkout_origin() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let (repo, _origin) = publication_repo();
            git(
                repo.path(),
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "https://github.com/Extra-Chill/blocks-everywhere.git",
                ],
            );
            let bin = home.path().join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let gh = bin.join("gh");
            std::fs::write(&gh, "#!/bin/sh\nprintf '%s' \"$GH_REPO\"\n").unwrap();
            std::fs::set_permissions(&gh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            let path = std::env::join_paths(
                std::iter::once(bin)
                    .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
            )
            .unwrap();
            let _path = homeboy_core::test_support::EnvVarGuard::set("PATH", path);
            let path = repo.path().to_str().unwrap();
            assert_eq!(publication_route(path).unwrap().route, "direct");
            let output = publication_gh_output(path, &["pr", "view", "139"]).unwrap();
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                "github.com/Extra-Chill/blocks-everywhere"
            );
        });
    }

    #[test]
    fn fork_policy_rejects_missing_owner_missing_remote_and_ambiguous_remote_before_push() {
        let (repo, origin) = publication_repo();
        let path = repo.path().to_str().unwrap();
        git(
            repo.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/WordPress/reprint.git",
            ],
        );
        let mut hosts = std::collections::HashMap::from([(
            "github.com".into(),
            homeboy_core::defaults::GitHostConfig {
                direct_owners: Some(vec![]),
                ..Default::default()
            },
        )]);
        let error = resolve_publication_route(path, &hosts).unwrap_err();
        assert!(error
            .message
            .contains("set git_hosts.github.com.fork_owner"));
        hosts.get_mut("github.com").unwrap().fork_owner = Some("chubes4".into());
        let error = resolve_publication_route(path, &hosts).unwrap_err();
        assert!(error
            .message
            .contains("gh repo fork WordPress/reprint --clone=false"));
        assert!(error
            .message
            .contains("git remote add chubes4 https://github.com/chubes4/reprint.git"));
        for remote in ["fork", "duplicate"] {
            git(
                repo.path(),
                &[
                    "remote",
                    "add",
                    remote,
                    "git@github.com:chubes4/reprint.git",
                ],
            );
        }
        let error = resolve_publication_route(path, &hosts).unwrap_err();
        assert!(error.message.contains("observed 2"));
        assert!(error.message.contains("git remote remove <duplicate>"));
        assert!(git_output(
            origin.path().to_str().unwrap(),
            &["show-ref", "--verify", "refs/heads/feature"]
        )
        .is_err());
    }

    #[test]
    fn manual_finalization_republishes_same_head_without_lifecycle_writes() {
        let (repo, _origin) = publication_repo();
        let path = repo.path().to_str().unwrap();
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
        let mut options = super::super::tests::options();
        options.path = path.into();
        options.manual_finalization = true;
        options.head = Some("feature".into());
        options.verified_base_sha = Some(git_output(path, &["rev-parse", "HEAD^"]).unwrap());
        options.verified_candidate_tree =
            Some(git_output(path, &["rev-parse", "HEAD^{tree}"]).unwrap());
        options.changed_files = vec!["feature.txt".into()];
        let run_id = options.run_id.clone();
        let mut backend = super::super::tests::MockBackend::default();
        backend.real_git = true;
        backend.branch = "feature".into();
        let first =
            super::super::finalize_pr_with_backend(options.clone(), &mut backend, &store).unwrap();
        backend.existing_pr = Some(AgentTaskPrRef {
            number: first.pr_number.unwrap(),
            url: first.pr_url.clone().unwrap(),
            is_draft: false,
        });
        let second = super::super::finalize_pr_with_backend(options, &mut backend, &store).unwrap();
        assert_eq!(first.status, "review_ready");
        assert_eq!(second.status, "review_ready");
        assert_eq!(first.pr_number, second.pr_number);
        assert_eq!(first.pr_url, second.pr_url);
        let first_sha = first
            .publication_proof
            .git_identity
            .unwrap()
            .commit_sha
            .unwrap();
        let second_sha = second
            .publication_proof
            .git_identity
            .unwrap()
            .commit_sha
            .unwrap();
        assert_eq!(first_sha, second_sha);
        assert_eq!(backend.create_calls, 1);
        assert!(
            store.read_record(&run_id).is_err(),
            "manual publication must not create lifecycle records"
        );
    }

    #[test]
    fn publishes_gated_tree_reproducibly_and_leaves_pr_open_on_read_failure() {
        let (repo, _origin) = publication_repo();
        let path = repo.path().to_str().unwrap();
        let base = git_output(path, &["rev-parse", "HEAD^"]).unwrap();
        let candidate_sha = git_output(path, &["rev-parse", "HEAD"]).unwrap();
        let tree = git_output(path, &["rev-parse", "HEAD^{tree}"]).unwrap();
        // Destination bytes are not publication input.
        std::fs::write(repo.path().join("unrelated"), "do not publish").unwrap();
        let mut options = super::super::tests::options();
        options.path = path.into();
        options.head = Some("fix/cook".into());
        options.verified_base_sha = Some(base.clone());
        options.candidate = Some(crate::agent_task::AgentTaskCandidate {
            base_sha: base.clone(),
            candidate_sha,
            tree_sha: tree.clone(),
        });
        options.changed_files = vec!["feature.txt".into()];
        let store =
            crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
        let mut backend = super::super::tests::MockBackend::default();
        backend.real_git = true;
        let first =
            super::super::finalize_pr_with_backend(options.clone(), &mut backend, &store).unwrap();
        assert!(backend.created);
        let sha = first
            .publication_proof
            .git_identity
            .unwrap()
            .commit_sha
            .unwrap();
        assert_eq!(
            git_output(path, &["rev-parse", &format!("{sha}^{{tree}}")]).unwrap(),
            tree
        );
        assert_eq!(
            git_output(path, &["rev-parse", &format!("{sha}^")]).unwrap(),
            base
        );
        backend.existing_pr = Some(AgentTaskPrRef {
            number: 123,
            url: "https://example.test/pr/123".into(),
            is_draft: false,
        });
        let second =
            super::super::finalize_pr_with_backend(options.clone(), &mut backend, &store).unwrap();
        assert_eq!(
            second
                .publication_proof
                .git_identity
                .unwrap()
                .commit_sha
                .as_deref(),
            Some(sha.as_str())
        );
        backend.read_failure = true;
        let error =
            super::super::finalize_pr_with_backend(options, &mut backend, &store).unwrap_err();
        assert_eq!(error.retryable, Some(true));
        assert!(error
            .message
            .contains("homeboy agent-task recover cook-3678"));
        assert!(!backend.existing_pr.unwrap().is_draft);
        assert_eq!(
            std::fs::read_to_string(repo.path().join("unrelated")).unwrap(),
            "do not publish"
        );
    }

    /// #17874: a promoted candidate is staged in the destination. Publishing
    /// it leaves the destination clean at the pushed head, so a follow-up
    /// Cook can attribute the worktree and removal is safe. #17346: a
    /// candidate that merged trunk publishes that merge, not a flat tree.
    #[test]
    fn publishing_a_staged_candidate_leaves_the_destination_clean_at_the_pushed_head() {
        let (repo, _origin) = publication_repo();
        let path = repo.path().to_str().unwrap();
        let base = git_output(path, &["rev-parse", "HEAD"]).unwrap();
        let trunk = git_output(
            path,
            &[
                "commit-tree",
                &format!("{base}^{{tree}}"),
                "-p",
                &base,
                "-m",
                "trunk moved",
            ],
        )
        .unwrap();
        std::fs::write(repo.path().join("feature.txt"), "candidate").unwrap();
        std::fs::write(repo.path().join("added.txt"), "added").unwrap();
        git(repo.path(), &["add", "-A"]);
        let mut backend = RealAgentTaskPrFinalizationBackend;

        homeboy_core::git::record_pending_merge(repo.path(), &[trunk.clone()]).unwrap();
        let (candidate, _) = backend.prepare_manual_candidate(path, &base).unwrap();
        let dates = backend.candidate_dates(path, &base).unwrap();
        let published = backend
            .publish_tree(path, &candidate, &base, "publish", &dates)
            .unwrap();
        let tracking = backend.push_branch(path, &published, "feature").unwrap();

        assert_eq!(
            git_output(
                path,
                &[
                    "rev-parse",
                    &format!("{published}^1"),
                    &format!("{published}^2")
                ]
            )
            .unwrap(),
            format!("{base}\n{trunk}")
        );
        assert_eq!(git_output(path, &["status", "--porcelain"]).unwrap(), "");
        assert!(git_output(path, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_err());
        assert_eq!(git_output(path, &["rev-parse", "HEAD"]).unwrap(), published);
        assert_eq!(tracking.verified_remote_sha, published);
    }

    #[test]
    fn pushed_branch_without_upstream_tracks_verified_remote_head() {
        let (repo, _origin) = publication_repo();
        let commit_sha = git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap();

        let tracking = RealAgentTaskPrFinalizationBackend
            .push_branch(repo.path().to_str().unwrap(), &commit_sha, "feature")
            .expect("branch published");

        assert_eq!(
            upstream(repo.path()).unwrap(),
            "refs/remotes/origin/feature"
        );
        assert_eq!(tracking.local_branch, "feature");
        assert_eq!(tracking.remote, "origin");
        assert_eq!(tracking.upstream_ref, "refs/remotes/origin/feature");
        assert_eq!(
            tracking.verified_remote_sha,
            git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap()
        );
        assert_eq!(
            git_output(repo.path().to_str().unwrap(), &["rev-parse", "@{upstream}"]).unwrap(),
            tracking.verified_remote_sha
        );
    }

    #[test]
    fn pushed_branch_replaces_stale_base_upstream_after_verification() {
        let (repo, _origin) = publication_repo();
        let commit_sha = git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap();
        git(
            repo.path(),
            &["branch", "--set-upstream-to", "origin/main", "feature"],
        );

        RealAgentTaskPrFinalizationBackend
            .push_branch(repo.path().to_str().unwrap(), &commit_sha, "feature")
            .expect("branch published");

        assert_eq!(
            upstream(repo.path()).unwrap(),
            "refs/remotes/origin/feature"
        );
        assert_eq!(
            git_output(repo.path().to_str().unwrap(), &["rev-parse", "@{upstream}"]).unwrap(),
            git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap()
        );
    }

    #[test]
    fn pushed_branch_preserves_already_correct_upstream() {
        let (repo, _origin) = publication_repo();
        git(repo.path(), &["push", "-u", "origin", "feature"]);
        std::fs::write(repo.path().join("second.txt"), "second").unwrap();
        git(repo.path(), &["add", "second.txt"]);
        git(repo.path(), &["commit", "-m", "second feature commit"]);
        let remote = git_output(
            repo.path().to_str().unwrap(),
            &["config", "branch.feature.remote"],
        )
        .unwrap();
        let merge = git_output(
            repo.path().to_str().unwrap(),
            &["config", "branch.feature.merge"],
        )
        .unwrap();
        let commit_sha = git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap();

        RealAgentTaskPrFinalizationBackend
            .push_branch(repo.path().to_str().unwrap(), &commit_sha, "feature")
            .expect("branch published");

        assert_eq!(
            upstream(repo.path()).unwrap(),
            "refs/remotes/origin/feature"
        );
        assert_eq!(
            git_output(repo.path().to_str().unwrap(), &["rev-parse", "@{upstream}"]).unwrap(),
            git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap()
        );
        assert_eq!(
            git_output(
                repo.path().to_str().unwrap(),
                &["config", "branch.feature.remote"]
            )
            .unwrap(),
            remote
        );
        assert_eq!(
            git_output(
                repo.path().to_str().unwrap(),
                &["config", "branch.feature.merge"]
            )
            .unwrap(),
            merge
        );
    }

    #[test]
    fn failed_push_does_not_mutate_branch_tracking() {
        let (repo, origin) = publication_repo();
        let commit_sha = git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap();
        // A rejecting pre-receive hook fails the push with non-transport
        // evidence, so the generic `git push failed` path is exercised.
        homeboy_test_support::write_executable(
            origin.path().join("hooks/pre-receive"),
            "#!/bin/sh\nexit 1\n",
            0o755,
        )
        .unwrap();

        let error = RealAgentTaskPrFinalizationBackend
            .push_branch(repo.path().to_str().unwrap(), &commit_sha, "feature")
            .expect_err("push fails");

        assert!(error.message.contains("git push failed"));
        assert!(upstream(repo.path()).is_err());
    }

    #[test]
    fn remote_head_read_honors_push_instead_of_over_the_fetch_url() {
        let _env = push_transport_env_lock();
        // The remote is addressed through an SSH-shaped fetch URL whose
        // transport is down; the operator routes pushes to a reachable local
        // destination with the same env-configured rewrite the host policy
        // would apply. The pre-publication remote-head read must follow the
        // effective push URL, not the fetch URL (#10734).
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "--bare", "-b", "main"]);
        let repo = repo();
        git(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                "git@git.example.test:acme/repo.git",
            ],
        );
        git(
            repo.path(),
            &["push", origin.path().to_str().unwrap(), "main"],
        );
        let path = repo.path().to_str().unwrap().to_string();
        let main_sha = git_output(&path, &["rev-parse", "main"]).unwrap();

        let fixture = tempfile::tempdir().unwrap();
        let ssh_fixture = fixture.path().join("unreachable-ssh");
        homeboy_test_support::write_executable(&ssh_fixture, "#!/bin/sh\nprintf '%s\\n' 'ssh: connect to host git.example.test port 22: Connection refused' >&2\nexit 255\n", 0o755)
        .unwrap();

        let _fixture_ssh = homeboy_core::test_support::EnvVarGuard::set(
            "GIT_SSH_COMMAND",
            ssh_fixture.to_str().unwrap(),
        );

        // While the fetch URL's transport is down, the read fails with the
        // same classified, retryable blocked action the push reports, with a
        // phase distinguishing the remote-head read from the push.
        let error = remote_branch_head(&path, "main").expect_err("fetch URL transport is down");
        assert_eq!(error.retryable, Some(true));
        let preflight = &error.details["git_push_preflight"];
        assert_eq!(preflight["schema"], "homeboy/git-push-preflight/v1");
        assert_eq!(preflight["phase"], "remote_head_read");
        assert_eq!(preflight["classification"], "transport_unreachable");
        assert_eq!(preflight["remote"], "origin");
        assert_eq!(preflight["transport_kind"], "ssh");
        assert_eq!(preflight["host"], "git.example.test");
        assert_eq!(
            preflight["effective_push_url"],
            "git@git.example.test:acme/repo.git"
        );
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("Restore reachability")));

        // Operator recovery: the rewrite routes the read to the effective
        // push destination, which is reachable, without Git ever contacting
        // the dead fetch URL.
        drop(_fixture_ssh);
        // Repo-local, not process-global GIT_CONFIG_* env: a partially set
        // env layer would break every git command concurrent tests spawn.
        git(
            repo.path(),
            &[
                "config",
                &format!("url.{}.pushInsteadOf", origin.path().display()),
                "git@git.example.test:acme/repo.git",
            ],
        );

        assert_eq!(
            remote_branch_head(&path, "main").expect("reads the effective push destination"),
            Some(main_sha)
        );
    }

    #[test]
    fn resolve_base_fetches_a_fresh_immutable_tracking_ref() {
        let repo = repo();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare"]);
        git(
            repo.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-u", "origin", "main"]);
        let clone = tempfile::tempdir().unwrap();
        assert!(Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                remote.path().to_str().unwrap(),
                clone.path().to_str().unwrap()
            ])
            .status()
            .unwrap()
            .success());
        git(clone.path(), &["config", "user.email", "test@example.com"]);
        git(clone.path(), &["config", "user.name", "Test"]);
        std::fs::write(clone.path().join("remote.txt"), "fresh").unwrap();
        git(clone.path(), &["add", "remote.txt"]);
        git(clone.path(), &["commit", "-m", "remote advance"]);
        git(clone.path(), &["push", "origin", "main"]);
        let remote_head =
            git_output(clone.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap();

        let resolved = RealAgentTaskPrFinalizationBackend
            .resolve_base(repo.path().to_str().unwrap(), "main")
            .expect("base fetched");

        assert_eq!(resolved.sha, remote_head.trim());
        assert_eq!(resolved.reference, "refs/homeboy/finalization/base/main");
    }

    #[test]
    fn resolve_verified_base_rejects_malformed_and_unresolvable_snapshots() {
        let repo = repo();
        for snapshot in ["not-a-sha", &"f".repeat(40)] {
            let error = RealAgentTaskPrFinalizationBackend
                .resolve_verified_base(repo.path().to_str().unwrap(), snapshot)
                .expect_err("invalid snapshot is rejected");
            assert_eq!(
                error.code,
                homeboy_core::ErrorCode::ValidationInvalidArgument
            );
        }
    }

    #[test]
    fn resolve_verified_base_rematerializes_only_the_persisted_snapshot() {
        let repo = repo();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare"]);
        git(
            repo.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-u", "origin", "main"]);
        git(repo.path(), &["checkout", "-b", "snapshot"]);
        std::fs::write(repo.path().join("snapshot.txt"), "snapshot").unwrap();
        git(repo.path(), &["add", "snapshot.txt"]);
        git(repo.path(), &["commit", "-m", "verified snapshot"]);
        let snapshot = git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap();
        git(repo.path(), &["push", "origin", "snapshot"]);

        let clone = tempfile::tempdir().unwrap();
        assert!(Command::new("git")
            .args([
                "clone",
                "--no-local",
                "--single-branch",
                "--branch",
                "main",
                remote.path().to_str().unwrap(),
                clone.path().to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success());
        assert!(git_output(
            clone.path().to_str().unwrap(),
            &["cat-file", "-e", &format!("{snapshot}^{{commit}}")],
        )
        .is_err());

        let resolved = RealAgentTaskPrFinalizationBackend
            .resolve_verified_base(clone.path().to_str().unwrap(), &snapshot)
            .expect("exact snapshot rematerialized");
        assert_eq!(resolved.sha, snapshot);
        assert!(git_output(
            clone.path().to_str().unwrap(),
            &["cat-file", "-e", &format!("{}^{{commit}}", resolved.sha)],
        )
        .is_ok());

        let unavailable = "f".repeat(40);
        let error = RealAgentTaskPrFinalizationBackend
            .resolve_verified_base(clone.path().to_str().unwrap(), &unavailable)
            .expect_err("unavailable exact snapshot is rejected");
        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::ValidationInvalidArgument
        );
    }
}
