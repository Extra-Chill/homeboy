use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use homeboy_core::component::Component;
use homeboy_core::deps;
use homeboy_core::error::{Error, Result};
use homeboy_core::git;

const REMOTE_REF_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Clone the source repository for exact-ref materialization.
///
/// A `git clone --local` hardlinks the object store, which fails with
/// `Invalid cross-device link` when the source and the Homeboy temp root live on
/// different filesystems (a common split, e.g. workspace on `/var/lib` and temp
/// on a mounted `/mnt/...`). Attempt the fast `--local` clone first, and on that
/// specific cross-device failure retry with `--no-hardlinks`, which copies
/// objects instead of linking them and works across devices (#9889).
fn clone_exact_ref_source(source_root: &Path, source_arg: &str, worktree_arg: &str) -> Result<()> {
    let local_result = git::run_git(
        source_root,
        &[
            "clone",
            "--no-checkout",
            "--local",
            source_arg,
            worktree_arg,
        ],
        "clone exact deploy ref source",
    );
    match local_result {
        Ok(_) => Ok(()),
        Err(error) if is_cross_device_link_error(&error) => git::run_git(
            source_root,
            &[
                "clone",
                "--no-checkout",
                "--no-hardlinks",
                source_arg,
                worktree_arg,
            ],
            "clone exact deploy ref source across filesystems",
        )
        .map(|_| ()),
        Err(error) => Err(error),
    }
}

/// Whether a git failure is the cross-device hardlink error produced by
/// `clone --local` when source and destination are on different filesystems.
fn is_cross_device_link_error(error: &Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("cross-device link")
        || (message.contains("failed to create link") && message.contains("invalid cross-device"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactRefIdentity {
    pub requested_ref: String,
    pub resolved_sha: String,
    pub source: String,
    pub resolution_mode: String,
}

pub(super) struct ExactRefCheckout {
    pub component: Component,
    pub identity: ExactRefIdentity,
    worktree_path: PathBuf,
}

pub(crate) fn resolve_exact_ref(
    component: &Component,
    requested_ref: &str,
) -> Result<ExactRefIdentity> {
    validate_component_source(component)?;
    let source_root = source_root(component)?;
    let resolution = resolve_commit(&source_root, requested_ref, component)?;
    Ok(ExactRefIdentity {
        requested_ref: requested_ref.to_string(),
        resolved_sha: resolution.sha,
        source: resolution.source,
        resolution_mode: resolution.mode,
    })
}

impl ExactRefCheckout {
    pub(crate) fn materialize(
        component: &Component,
        requested_ref: &str,
        accepted_sha: Option<&str>,
    ) -> Result<Self> {
        let source_root = source_root(component)?;
        let identity = match accepted_sha {
            Some(resolved_sha) => ExactRefIdentity {
                requested_ref: requested_ref.to_string(),
                resolved_sha: resolved_sha.to_string(),
                source: source_root.to_string_lossy().to_string(),
                resolution_mode: "release-set-preflight".to_string(),
            },
            None => resolve_exact_ref(component, requested_ref)?,
        };
        ensure_resolved_commit_is_available(&source_root, component, &identity)?;
        let component_prefix = git::get_component_path_prefix(&component.local_path);
        let parent = std::env::temp_dir().join("homeboy-deploy-ref");
        std::fs::create_dir_all(&parent).map_err(|err| {
            Error::internal_io(
                format!("Failed to create exact-ref deploy temp directory: {err}"),
                Some("deploy.ref.temp".to_string()),
            )
        })?;
        let worktree_path = parent.join(uuid::Uuid::new_v4().to_string());
        let worktree_arg = worktree_path.to_string_lossy().to_string();
        clone_exact_ref_source(
            &source_root,
            source_root.to_str().unwrap_or_default(),
            &worktree_arg,
        )?;
        git::run_git(
            &worktree_path,
            &["checkout", "--detach", &identity.resolved_sha],
            "materialize exact deploy ref",
        )?;

        let materialized_path = component_prefix
            .as_deref()
            .map(|prefix| worktree_path.join(prefix))
            .unwrap_or_else(|| worktree_path.clone());
        if !materialized_path.exists() {
            let mut checkout = Self {
                component: component.clone(),
                identity,
                worktree_path,
            };
            checkout.cleanup();
            return Err(Error::validation_invalid_argument(
                "ref",
                format!(
                    "Resolved ref does not contain the declared component path for '{}'",
                    component.id
                ),
                None,
                None,
            ));
        }

        let mut materialized = component.clone();
        materialized.local_path = materialized_path.to_string_lossy().to_string();
        materialized.build_artifact = component.build_artifact.as_deref().map(|artifact| {
            rebase_source_path(
                artifact,
                Path::new(&component.local_path),
                &materialized_path,
            )
        });
        Ok(Self {
            component: materialized,
            identity,
            worktree_path,
        })
    }

    pub(crate) fn verify(&self) -> Result<()> {
        let actual_sha = git::run_git(
            &self.worktree_path,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            "verify exact deploy ref worktree",
        )?
        .trim()
        .to_string();
        if actual_sha != self.identity.resolved_sha {
            return Err(Error::validation_invalid_argument(
                "ref",
                format!(
                    "Materialized source verification failed for component '{}': expected commit '{}', found '{}'",
                    self.component.id, self.identity.resolved_sha, actual_sha
                ),
                None,
                None,
            ));
        }
        Ok(())
    }

    /// Hydrate dependencies in the detached source tree, never the configured
    /// checkout. This makes an exact-ref build self-contained.
    pub(crate) fn hydrate_dependencies(
        &self,
        skip: bool,
    ) -> Result<Option<deps::DependencyInstallResult>> {
        let started = Instant::now();
        let path = Path::new(&self.component.local_path);
        if skip {
            homeboy_core::log_status!(
                "deploy",
                "phase=exact_ref_hydration_skipped component={} ref={} commit={} cwd={} reason=--skip-deps-hydration duration_ms={}",
                self.component.id,
                self.identity.requested_ref,
                self.identity.resolved_sha,
                path.display(),
                started.elapsed().as_millis()
            );
            return Ok(None);
        }
        homeboy_core::log_status!(
            "deploy",
            "phase=exact_ref_hydration component={} ref={} commit={} cwd={}",
            self.component.id,
            self.identity.requested_ref,
            self.identity.resolved_sha,
            path.display()
        );

        match deps::install_for_resolved(&self.component, path)? {
            Some(result) => {
                for install in &result.installs {
                    homeboy_core::log_status!(
                        "deploy",
                        "phase=exact_ref_hydration component={} ref={} commit={} provider={} cwd={} command={} status={:?}",
                        self.component.id,
                        self.identity.requested_ref,
                        self.identity.resolved_sha,
                        result.package_manager,
                        result.component_path,
                        homeboy_core::redaction::redact_string(&install.command.join(" ")),
                        install.status
                    );
                }
                homeboy_core::log_status!(
                    "deploy",
                    "phase=exact_ref_hydration_complete component={} ref={} commit={} cwd={} duration_ms={}",
                    self.component.id,
                    self.identity.requested_ref,
                    self.identity.resolved_sha,
                    result.component_path,
                    started.elapsed().as_millis()
                );
                Ok(Some(result))
            }
            None => {
                homeboy_core::log_status!(
                    "deploy",
                    "phase=exact_ref_hydration_skipped component={} ref={} commit={} cwd={} reason=no_dependency_provider duration_ms={}",
                    self.component.id,
                    self.identity.requested_ref,
                    self.identity.resolved_sha,
                    path.display(),
                    started.elapsed().as_millis()
                );
                Ok(None)
            }
        }
    }

    fn cleanup(&mut self) {
        let _ = std::fs::remove_dir_all(&self.worktree_path);
    }
}

fn ensure_resolved_commit_is_available(
    source_root: &Path,
    component: &Component,
    identity: &ExactRefIdentity,
) -> Result<()> {
    if git::run_git(
        source_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", identity.resolved_sha),
        ],
        "check exact deploy ref availability",
    )
    .is_ok()
    {
        return Ok(());
    }

    let remote = git::resolve_default_remote(source_root);
    let remote_url = git::remote_url(source_root, &remote)
        .ok_or_else(|| unresolvable_ref(&identity.requested_ref, source_root, &component.id))?;
    let transport_env = component_transport_env(component, &remote_url);
    homeboy_core::log_status!(
        "deploy",
        "phase=exact_ref_fetch component={} ref={} commit={}",
        component.id,
        identity.requested_ref,
        identity.resolved_sha
    );
    git::run_git_with_env_timeout(
        source_root,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            &remote,
            &format!("+{}:", identity.resolved_sha),
        ],
        "fetch preflighted exact deploy ref",
        &transport_env,
        REMOTE_REF_QUERY_TIMEOUT,
    )
    .map_err(|error| remote_transport_error(&remote, &component.id, &error))?;
    let fetched = git::run_git(
        source_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", identity.resolved_sha),
        ],
        "verify fetched preflighted exact deploy ref",
    )
    .map_err(|_| unresolvable_ref(&identity.requested_ref, source_root, &component.id))?;
    if fetched.trim() == identity.resolved_sha {
        Ok(())
    } else {
        Err(identity_mismatch(&identity.requested_ref, &component.id))
    }
}

fn rebase_source_path(value: &str, original_root: &Path, materialized_root: &Path) -> String {
    let path = Path::new(value);
    if !path.is_absolute() {
        return value.to_string();
    }

    path.strip_prefix(original_root)
        .map(|relative| {
            materialized_root
                .join(relative)
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_else(|_| value.to_string())
}

impl Drop for ExactRefCheckout {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn validate_component_source(component: &Component) -> Result<()> {
    if component.is_file_component() {
        return Err(unsupported_source(
            component,
            "file deploy sources are not Git trees",
        ));
    }
    if component.deploy_config().is_git_deploy() {
        return Err(unsupported_source(
            component,
            "deploy_strategy 'git' updates a remote checkout instead of packaging the selected tree",
        ));
    }
    Ok(())
}

fn unsupported_source(component: &Component, reason: &str) -> Error {
    Error::validation_invalid_argument(
        "ref",
        format!(
            "Component '{}' does not support --ref: {reason}",
            component.id
        ),
        None,
        None,
    )
}

fn source_root(component: &Component) -> Result<PathBuf> {
    git::get_git_root(&component.local_path)
        .map(PathBuf::from)
        .map_err(|_| {
            Error::validation_invalid_argument(
                "ref",
                format!(
                    "Cannot use --ref for component '{}': declared source '{}' is not a Git repository",
                    component.id, component.local_path
                ),
                None,
                None,
            )
        })
}

struct ResolvedCommit {
    sha: String,
    source: String,
    mode: String,
}

fn resolve_commit(
    source_root: &Path,
    requested_ref: &str,
    component: &Component,
) -> Result<ResolvedCommit> {
    if requested_ref.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "ref",
            "--ref must not be empty",
            None,
            None,
        ));
    }
    let commit_ref = format!("{requested_ref}^{{commit}}");
    if let Ok(sha) = git::run_git(
        source_root,
        &["rev-parse", "--verify", &commit_ref],
        "resolve exact deploy ref",
    ) {
        return Ok(ResolvedCommit {
            sha: sha.trim().to_string(),
            source: source_root.to_string_lossy().to_string(),
            mode: "local".to_string(),
        });
    }

    resolve_remote_commit(source_root, requested_ref, component)
}

fn resolve_remote_commit(
    source_root: &Path,
    requested_ref: &str,
    component: &Component,
) -> Result<ResolvedCommit> {
    let component_id = &component.id;
    let remote = git::resolve_default_remote(source_root);
    let Some(remote_url) = git::remote_url(source_root, &remote) else {
        return Err(unresolvable_ref(requested_ref, source_root, component_id));
    };
    let transport_env = component_transport_env(component, &remote_url);

    let (sha, _remote_ref) = resolve_named_remote_ref(
        source_root,
        &remote,
        requested_ref,
        component_id,
        &transport_env,
    )?;
    if is_full_sha(requested_ref) && !sha.eq_ignore_ascii_case(requested_ref) {
        return Err(identity_mismatch(requested_ref, component_id));
    }
    Ok(ResolvedCommit {
        sha,
        source: format!("remote:{remote}"),
        mode: if is_full_sha(requested_ref) {
            "remote_sha".to_string()
        } else {
            "remote_named_ref".to_string()
        },
    })
}

fn resolve_named_remote_ref(
    source_root: &Path,
    remote: &str,
    requested_ref: &str,
    component_id: &str,
    transport_env: &[(String, String)],
) -> Result<(String, String)> {
    let args = if is_full_sha(requested_ref) {
        vec!["ls-remote", "--refs", remote]
    } else {
        vec!["ls-remote", "--refs", remote, requested_ref]
    };
    homeboy_core::log_status!(
        "deploy",
        "phase=exact_ref_remote_query component={} ref={}",
        component_id,
        requested_ref
    );
    let output = git::run_git_with_env_timeout(
        source_root,
        &args,
        "query named exact deploy ref",
        transport_env,
        REMOTE_REF_QUERY_TIMEOUT,
    )
    .map_err(|err| remote_transport_error(remote, component_id, &err))?;
    let candidates: Vec<(&str, &str)> = output
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .filter_map(|(sha, reference)| {
            let reference = reference.trim();
            (is_full_sha(requested_ref) && sha.eq_ignore_ascii_case(requested_ref)
                || remote_ref_matches(reference, requested_ref))
            .then_some((sha, reference))
        })
        .collect();
    match candidates.as_slice() {
        [(sha, reference)] => Ok(((*sha).to_string(), (*reference).to_string())),
        candidates if is_full_sha(requested_ref)
            && candidates.iter().all(|(sha, _)| sha.eq_ignore_ascii_case(requested_ref)) =>
        {
            Ok((requested_ref.to_string(), candidates[0].1.to_string()))
        }
        [] => Err(unresolvable_ref(requested_ref, source_root, component_id)),
        _ => Err(Error::validation_invalid_argument(
            "ref",
            format!(
                "Cannot resolve --ref '{}' unambiguously from declared Git remote '{}' for component '{}'",
                requested_ref, remote, component_id
            ),
            None,
            None,
        )),
    }
}

fn component_transport_env(component: &Component, remote_url: &str) -> Vec<(String, String)> {
    remote_host(remote_url)
        .map(|host| homeboy_core::git::github_cli_env(&host, &component.github))
        .unwrap_or_default()
}

fn remote_host(remote_url: &str) -> Option<String> {
    let remote_url = remote_url.trim();
    let authority = remote_url
        .strip_prefix("https://")
        .or_else(|| remote_url.strip_prefix("http://"))
        .or_else(|| remote_url.strip_prefix("ssh://"))
        .or_else(|| remote_url.split_once('@').map(|(_, value)| value))?;
    let host = authority
        .split('@')
        .next_back()?
        .split('/')
        .next()?
        .split(':')
        .next()?
        .trim();
    (!host.is_empty()).then(|| host.to_string())
}

fn remote_ref_matches(reference: &str, requested_ref: &str) -> bool {
    reference == requested_ref
        || reference == format!("refs/heads/{requested_ref}")
        || reference == format!("refs/tags/{requested_ref}")
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn unresolvable_ref(requested_ref: &str, source_root: &Path, component_id: &str) -> Error {
    Error::validation_invalid_argument(
        "ref",
        format!(
            "Cannot resolve --ref '{}' to an unambiguous commit in declared repository '{}' for component '{}'",
            requested_ref,
            source_root.display(),
            component_id
        ),
        None,
        None,
    )
}

fn identity_mismatch(requested_ref: &str, component_id: &str) -> Error {
    Error::validation_invalid_argument(
        "ref",
        format!(
            "Declared Git remote resolved exact SHA '{}' to a different commit for component '{}'",
            requested_ref, component_id
        ),
        None,
        Some(vec![
            "Use the immutable commit SHA returned by the repository, then retry the deploy."
                .to_string(),
        ]),
    )
}

fn remote_transport_error(remote: &str, component_id: &str, error: &Error) -> Error {
    let detail = error
        .details
        .get("stderr")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = if [
        "authentication",
        "authorization",
        "could not read username",
        "terminal prompts disabled",
        "http 401",
        "http 403",
    ]
    .iter()
    .any(|needle| detail.contains(needle))
    {
        format!(
            "Git authentication failed while querying declared remote '{}' for component '{}'",
            remote, component_id
        )
    } else if [
        "could not resolve host",
        "connection refused",
        "connection timed out",
        "network is unreachable",
        "proxy",
    ]
    .iter()
    .any(|needle| detail.contains(needle))
    {
        format!(
            "Git connectivity failed while querying declared remote '{}' for component '{}'",
            remote, component_id
        )
    } else {
        format!(
            "Unable to query declared Git remote '{}' for component '{}'",
            remote, component_id
        )
    };
    Error::validation_invalid_argument(
        "ref",
        message,
        None,
        Some(vec!["Check the remote URL, configured Git authentication, and transport proxy settings. Git credentials are not included in this error.".to_string()]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn exact_sha_branch_and_tag_resolve_to_immutable_commit() {
        let repo = fixture_repo();
        let component = fixture_component(repo.path());
        let sha = git_output(repo.path(), &["rev-parse", "HEAD"]);

        assert_eq!(
            resolve_exact_ref(&component, &sha)
                .expect("exact SHA")
                .resolved_sha,
            sha
        );
        assert_eq!(
            resolve_exact_ref(&component, "accepted")
                .expect("branch")
                .resolved_sha,
            sha
        );
        assert_eq!(
            resolve_exact_ref(&component, "v1.0.0")
                .expect("tag")
                .resolved_sha,
            sha
        );
    }

    #[test]
    fn remote_host_supports_https_and_ssh_transports() {
        assert_eq!(
            remote_host("https://git.example.test/owner/repo.git"),
            Some("git.example.test".to_string())
        );
        assert_eq!(
            remote_host("ssh://git@git.example.test:2222/owner/repo.git"),
            Some("git.example.test".to_string())
        );
        assert_eq!(
            remote_host("git@git.example.test:owner/repo.git"),
            Some("git.example.test".to_string())
        );
    }

    #[test]
    fn exact_ref_transport_uses_component_host_policy_without_exposing_values() {
        let mut component = Component::default();
        component.github.hosts.insert(
            "git.example.test".to_string(),
            homeboy_core::component::GithubHostConfig {
                proxy: Some("socks5://127.0.0.1:9911".to_string()),
                env: std::collections::HashMap::from([(
                    "GIT_ASKPASS".to_string(),
                    "/private/credential-helper".to_string(),
                )]),
            },
        );

        let env = component_transport_env(&component, "https://git.example.test/acme/repo.git");

        assert!(env.contains(&(
            "HTTPS_PROXY".to_string(),
            "socks5://127.0.0.1:9911".to_string()
        )));
        assert!(env.contains(&(
            "GIT_ASKPASS".to_string(),
            "/private/credential-helper".to_string()
        )));
    }

    #[test]
    fn materialized_ref_is_independent_of_stale_checkout_head_and_does_not_mutate_it() {
        let repo = fixture_repo();
        let component = fixture_component(repo.path());
        let accepted_sha = git_output(repo.path(), &["rev-parse", "accepted"]);
        std::fs::write(repo.path().join("payload.txt"), "newer checkout\n").expect("write");
        git(repo.path(), &["add", "payload.txt"]);
        commit(repo.path(), "new checkout head");
        let checkout_head = git_output(repo.path(), &["rev-parse", "HEAD"]);
        assert_ne!(accepted_sha, checkout_head);

        let worktree_path = {
            let checkout = ExactRefCheckout::materialize(&component, "accepted", None)
                .expect("materialize accepted branch");
            assert_eq!(checkout.identity.resolved_sha, accepted_sha);
            assert_eq!(
                std::fs::read_to_string(
                    Path::new(&checkout.component.local_path).join("payload.txt")
                )
                .expect("materialized payload"),
                "accepted\n"
            );
            PathBuf::from(&checkout.component.local_path)
        };

        assert!(
            !worktree_path.exists(),
            "temporary worktree should be removed"
        );
        assert_eq!(
            git_output(repo.path(), &["rev-parse", "HEAD"]),
            checkout_head
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("payload.txt")).expect("checkout payload"),
            "newer checkout\n"
        );
    }

    #[test]
    fn dry_resolution_is_read_only_and_unresolvable_refs_fail_clearly() {
        let repo = fixture_repo();
        let component = fixture_component(repo.path());
        let before = git_output(repo.path(), &["status", "--porcelain=v1"]);
        let identity = resolve_exact_ref(&component, "accepted").expect("resolve branch");

        assert_eq!(identity.requested_ref, "accepted");
        assert_eq!(
            Path::new(&identity.source)
                .canonicalize()
                .expect("canonical source"),
            repo.path().canonicalize().expect("canonical repo")
        );
        assert_eq!(
            git_output(repo.path(), &["status", "--porcelain=v1"]),
            before
        );
        let err =
            resolve_exact_ref(&component, "missing-ref").expect_err("missing ref should fail");
        assert!(err.message.contains("Cannot resolve --ref 'missing-ref'"));
        assert!(err.message.contains(&identity.source));
    }

    #[test]
    fn stale_checkout_fetches_exact_sha_and_named_ref_without_changing_head_or_index() {
        let fixture = remote_fixture();
        let sha_checkout = stale_clone(&fixture);
        let sha_component = fixture_component(&sha_checkout);
        let sha_head = git_output(&sha_checkout, &["rev-parse", "HEAD"]);
        let sha_index = git_output(&sha_checkout, &["write-tree"]);

        let sha_identity = resolve_exact_ref(&sha_component, &fixture.target_sha)
            .expect("fetch exact SHA from remote");
        assert_eq!(sha_identity.resolved_sha, fixture.target_sha);
        assert_eq!(sha_identity.source, "remote:origin");
        assert_eq!(sha_identity.resolution_mode, "remote_sha");
        assert_eq!(git_output(&sha_checkout, &["rev-parse", "HEAD"]), sha_head);
        assert_eq!(git_output(&sha_checkout, &["write-tree"]), sha_index);

        let named_checkout = stale_clone(&fixture);
        let named_component = fixture_component(&named_checkout);
        let named_head = git_output(&named_checkout, &["rev-parse", "HEAD"]);
        let named_index = git_output(&named_checkout, &["write-tree"]);
        let named_state = materialization_source_state(&named_checkout);
        let checkout = ExactRefCheckout::materialize(&named_component, "accepted", None)
            .expect("fetch and materialize named remote ref");
        assert_eq!(checkout.identity.resolved_sha, fixture.target_sha);
        assert_eq!(checkout.identity.resolution_mode, "remote_named_ref");
        assert_eq!(
            git_output(&named_checkout, &["rev-parse", "HEAD"]),
            named_head
        );
        assert_eq!(git_output(&named_checkout, &["write-tree"]), named_index);
        assert_eq!(materialization_source_state(&named_checkout), named_state);
    }

    #[test]
    fn batch_preflight_leaves_every_checkout_unchanged_when_a_later_ref_fails() {
        let fixture = remote_fixture();
        let remote_only_checkout = stale_clone(&fixture);
        let remote_only_component = fixture_component(&remote_only_checkout);
        let failing_repo = fixture_repo();
        let failing_component = fixture_component(failing_repo.path());
        let before = [&remote_only_checkout, failing_repo.path()]
            .into_iter()
            .map(git_state_snapshot)
            .collect::<Vec<_>>();

        let error = crate::deploy::preflight_exact_refs(&[
            (&remote_only_component, "accepted"),
            (&failing_component, "missing-ref"),
        ])
        .expect_err("a later failed member must reject the complete preflight");

        assert!(error.message.contains("missing-ref"));
        for (path, expected) in [&remote_only_checkout, failing_repo.path()]
            .into_iter()
            .zip(before)
        {
            assert_eq!(git_state_snapshot(path), expected);
        }
    }

    #[test]
    fn preflighted_sha_is_materialized_after_the_requested_branch_moves() {
        let fixture = remote_fixture();
        let checkout = stale_clone(&fixture);
        let component = fixture_component(&checkout);
        let accepted_sha = crate::deploy::preflight_exact_refs(&[(&component, "accepted")])
            .expect("preflight accepted branch")
            .remove("fixture")
            .expect("accepted SHA");

        std::fs::write(checkout.join("payload.txt"), "moved\n").expect("moved payload");
        git(&checkout, &["add", "payload.txt"]);
        commit(&checkout, "move accepted branch");
        let moved_sha = git_output(&checkout, &["rev-parse", "HEAD"]);
        git(
            &checkout,
            &["push", "-q", "--force", "origin", "HEAD:accepted"],
        );
        assert_ne!(accepted_sha, moved_sha);

        let materialized =
            ExactRefCheckout::materialize(&component, "accepted", Some(&accepted_sha))
                .expect("materialize preflighted commit");
        assert_eq!(materialized.identity.resolved_sha, accepted_sha);
        assert_eq!(
            std::fs::read_to_string(
                Path::new(&materialized.component.local_path).join("payload.txt")
            )
            .expect("materialized payload"),
            "accepted\n"
        );
    }

    #[test]
    fn missing_remote_ref_and_transport_failure_are_actionable() {
        let fixture = remote_fixture();
        let checkout = stale_clone(&fixture);
        let component = fixture_component(&checkout);
        let missing = resolve_exact_ref(&component, "missing-ref").expect_err("missing ref");
        assert!(missing
            .message
            .contains("Cannot resolve --ref 'missing-ref'"));

        git(
            &checkout,
            &["remote", "set-url", "origin", "/missing/remote.git"],
        );
        let transport =
            resolve_exact_ref(&component, "also-missing").expect_err("transport failure");
        assert!(transport
            .message
            .contains("Unable to query declared Git remote"));
        assert!(transport.message.contains("remote 'origin'"));
    }

    #[test]
    fn materialized_ref_verification_rejects_a_different_checkout_head() {
        let repo = fixture_repo();
        let component = fixture_component(repo.path());
        std::fs::write(repo.path().join("other.txt"), "other\n").expect("other payload");
        git(repo.path(), &["add", "other.txt"]);
        commit(repo.path(), "other commit");
        let other_sha = git_output(repo.path(), &["rev-parse", "HEAD"]);
        let checkout = ExactRefCheckout::materialize(&component, "accepted", None)
            .expect("materialize accepted branch");
        git(
            Path::new(&checkout.component.local_path),
            &["checkout", "--detach", &other_sha],
        );

        let error = checkout
            .verify()
            .expect_err("changed materialized HEAD must fail closed");
        assert!(error
            .message
            .contains("Materialized source verification failed"));
    }

    #[test]
    fn hydration_failure_is_actionable_and_removes_only_the_temporary_checkout() {
        let repo = fixture_repo();
        std::fs::write(
            repo.path().join("homeboy-deps.json"),
            r#"{"provider":"failing-fixture","commands":{"install":{"argv":["sh","-c","exit 42"]}}}"#,
        )
        .expect("dependency provider");
        git(repo.path(), &["add", "homeboy-deps.json"]);
        commit(repo.path(), "add failing provider");
        let component = fixture_component(repo.path());

        let temporary_path = {
            let checkout = ExactRefCheckout::materialize(&component, "HEAD", None)
                .expect("materialize exact ref");
            let temporary_path = PathBuf::from(&checkout.component.local_path);
            let error = checkout
                .hydrate_dependencies(false)
                .expect_err("failed hydration must stop before build or deploy");
            assert!(error.message.contains("Dependency provider install failed"));
            assert!(format!("{error:?}").contains("Run manually in"));
            temporary_path
        };

        assert!(
            !temporary_path.exists(),
            "temporary checkout is removed on failure"
        );
        assert!(
            repo.path().join("homeboy-deps.json").exists(),
            "configured checkout is never cleaned"
        );
    }

    #[test]
    fn hydration_opt_out_does_not_run_the_materialized_provider() {
        let repo = fixture_repo();
        std::fs::write(
            repo.path().join("homeboy-deps.json"),
            r#"{"provider":"failing-fixture","commands":{"install":{"argv":["sh","-c","exit 42"]}}}"#,
        )
        .expect("dependency provider");
        git(repo.path(), &["add", "homeboy-deps.json"]);
        commit(repo.path(), "add failing provider");
        let component = fixture_component(repo.path());
        let checkout =
            ExactRefCheckout::materialize(&component, "HEAD", None).expect("materialize");

        assert!(checkout
            .hydrate_dependencies(true)
            .expect("explicit opt-out must skip hydration")
            .is_none());
    }

    #[test]
    fn exact_ref_rejects_incompatible_deploy_source_types() {
        let repo = fixture_repo();
        let mut component = fixture_component(repo.path());
        component.deploy_strategy = Some("git".to_string());
        let git_error = resolve_exact_ref(&component, "accepted")
            .expect_err("git deploy strategy should be rejected");
        assert!(git_error
            .message
            .contains("deploy_strategy 'git' updates a remote checkout"));

        component.deploy_strategy = Some("file".to_string());
        component.local_path = repo
            .path()
            .join("payload.txt")
            .to_string_lossy()
            .to_string();
        let file_error = resolve_exact_ref(&component, "accepted")
            .expect_err("file deploy strategy should be rejected");
        assert!(file_error.message.contains("file deploy sources"));
    }

    #[test]
    fn cross_device_link_error_is_detected() {
        // Mirrors the real run_git failure, whose Display message carries the
        // git stderr detail (see git_failure_message).
        let cross_device = Error::internal_unexpected(
            "clone exact deploy ref source failed: fatal: failed to create link '/tmp/x/.git/objects/aa': Invalid cross-device link",
        );
        assert!(is_cross_device_link_error(&cross_device));

        // An unrelated git failure must not trigger the --no-hardlinks retry.
        let other = Error::internal_unexpected(
            "clone exact deploy ref source failed: fatal: repository not found",
        );
        assert!(!is_cross_device_link_error(&other));
    }

    #[test]
    fn clone_exact_ref_source_succeeds_on_same_filesystem() {
        // Happy path: --local clone within one filesystem (the tempdir) works
        // without needing the cross-device fallback.
        let repo = fixture_repo();
        let dest = tempfile::tempdir().expect("dest parent");
        let worktree = dest.path().join("clone");
        clone_exact_ref_source(
            repo.path(),
            repo.path().to_str().unwrap(),
            worktree.to_str().unwrap(),
        )
        .expect("same-filesystem clone succeeds");
        assert!(worktree.join(".git").exists());
    }

    fn fixture_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init", "-q"]);
        git(repo.path(), &["config", "user.name", "Homeboy Test"]);
        git(
            repo.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        std::fs::write(repo.path().join("payload.txt"), "accepted\n").expect("payload");
        git(repo.path(), &["add", "payload.txt"]);
        commit(repo.path(), "accepted");
        git(repo.path(), &["branch", "accepted"]);
        git(repo.path(), &["tag", "v1.0.0"]);
        repo
    }

    fn fixture_component(path: &Path) -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: path.to_string_lossy().to_string(),
            build_artifact: Some("build/fixture.zip".to_string()),
            ..Component::default()
        }
    }

    struct RemoteFixture {
        _root: tempfile::TempDir,
        remote: PathBuf,
        target_sha: String,
    }

    fn remote_fixture() -> RemoteFixture {
        let root = tempfile::tempdir().expect("fixture root");
        let remote = root.path().join("remote.git");
        let seed = root.path().join("seed");
        git(
            root.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        git(
            root.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        git(&seed, &["config", "user.name", "Homeboy Test"]);
        git(&seed, &["config", "user.email", "homeboy@example.test"]);
        std::fs::write(seed.join("payload.txt"), "stale\n").expect("stale payload");
        git(&seed, &["add", "payload.txt"]);
        commit(&seed, "stale");
        git(&seed, &["push", "-q", "origin", "main"]);
        let stale_clone = root.path().join("stale-template");
        git(
            root.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                stale_clone.to_str().unwrap(),
            ],
        );
        std::fs::write(seed.join("payload.txt"), "accepted\n").expect("accepted payload");
        git(&seed, &["add", "payload.txt"]);
        commit(&seed, "accepted");
        let target_sha = git_output(&seed, &["rev-parse", "HEAD"]);
        git(&seed, &["branch", "accepted"]);
        git(&seed, &["push", "-q", "origin", "main", "accepted"]);
        RemoteFixture {
            _root: root,
            remote,
            target_sha,
        }
    }

    fn stale_clone(fixture: &RemoteFixture) -> PathBuf {
        let checkout = fixture
            .remote
            .parent()
            .expect("fixture parent")
            .join(uuid::Uuid::new_v4().to_string());
        let stale_template = fixture
            .remote
            .parent()
            .expect("fixture parent")
            .join("stale-template");
        git(
            fixture.remote.parent().expect("fixture parent"),
            &[
                "clone",
                "-q",
                stale_template.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        git(
            &checkout,
            &[
                "remote",
                "set-url",
                "origin",
                fixture.remote.to_str().unwrap(),
            ],
        );
        checkout
    }

    fn commit(path: &Path, message: &str) {
        git(path, &["commit", "-q", "-m", message]);
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    }

    fn git_state_snapshot(path: &Path) -> (String, String, Option<Vec<u8>>) {
        let fetch_head = git_output(path, &["rev-parse", "--git-path", "FETCH_HEAD"]);
        (
            git_output(path, &["status", "--porcelain=v1"]),
            git_output(path, &["worktree", "list", "--porcelain"]),
            std::fs::read(path.join(fetch_head)).ok(),
        )
    }

    fn materialization_source_state(
        path: &Path,
    ) -> (String, String, String, String, String, Option<Vec<u8>>) {
        let fetch_head = git_output(path, &["rev-parse", "--git-path", "FETCH_HEAD"]);
        (
            git_output(path, &["for-each-ref", "--format=%(refname) %(objectname)"]),
            git_output(path, &["status", "--porcelain=v1"]),
            git_output(path, &["rev-parse", "HEAD"]),
            git_output(path, &["write-tree"]),
            git_output(path, &["worktree", "list", "--porcelain"]),
            std::fs::read(path.join(fetch_head)).ok(),
        )
    }
}
