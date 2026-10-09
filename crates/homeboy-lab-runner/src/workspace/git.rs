use std::path::Path;
use std::process::Command;

use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};

use super::super::{Runner, RunnerKind};
use super::control::WorkspaceControl;
use super::materializer::{WorkspaceMaterializationOperation, WorkspaceMaterializer};
use super::types::ControllerGitBundleProvenance;
use super::types::GitSnapshot;
use super::util::{ssh_args, ssh_client_for_runner, verify_valid_git_representation};

#[cfg(test)]
pub(super) fn git_snapshot(
    local_path: &Path,
    changed_since_base: Option<&str>,
    git_fetch_refs: Vec<String>,
    controller_routed_git: bool,
) -> Result<GitSnapshot> {
    git_snapshot_controlled(
        local_path,
        changed_since_base,
        git_fetch_refs,
        controller_routed_git,
        &WorkspaceControl::default(),
    )
}

pub(super) fn git_snapshot_controlled(
    local_path: &Path,
    changed_since_base: Option<&str>,
    git_fetch_refs: Vec<String>,
    controller_routed_git: bool,
    control: &WorkspaceControl,
) -> Result<GitSnapshot> {
    control.checkpoint()?;
    let head = control.git(local_path, &["rev-parse", "HEAD"])?;
    let branch = control
        .git(local_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|branch| branch != "HEAD");
    let remote_url = control.git(local_path, &["config", "--get", "remote.origin.url"])?;
    if remote_url.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "remote.origin.url",
            "git workspace sync requires remote.origin.url",
            None,
            None,
        ));
    }
    super::super::source_materialization::validate_sanitized_git_remote(&remote_url)?;
    if controller_routed_git
        || branch.is_none()
        || super::super::source_materialization::requires_controller_routed_workspace_sync(
            &remote_url,
        )
        // A partial controller checkout may need its selected HEAD hydrated
        // before Git can even evaluate cleanliness.
        || promisor_remote(local_path, control)?.is_some()
    {
        let refs = controller_bundle_refs(&head, changed_since_base, &git_fetch_refs);
        repair_controller_bundle_commit_closure(local_path, &refs, control)?;
    }
    ensure_clean_git_working_tree(local_path, changed_since_base, control)?;

    Ok(GitSnapshot {
        remote_url,
        head,
        branch,
        changed_since_base: changed_since_base.map(str::to_string),
        git_fetch_refs,
    })
}

fn ensure_clean_git_working_tree(
    local_path: &Path,
    changed_since_base: Option<&str>,
    control: &WorkspaceControl,
) -> Result<()> {
    let status = control.git(local_path, &["status", "--porcelain=v1"])?;
    if !status.trim().is_empty() {
        if changed_since_base.is_some() {
            return Err(Error::validation_invalid_argument(
                "mode",
                "git workspace sync requires a clean working tree for changed-since remote execution; snapshot sync cannot honor --changed-since because it excludes .git metadata",
                Some("git".to_string()),
                Some(vec![
                    "Commit or stash local changes before remote execution of a --changed-since command."
                        .to_string(),
                    "Run with --placement local to execute the changed-since command locally."
                        .to_string(),
                    "Omit --changed-since to use snapshot remote execution for dirty local changes."
                        .to_string(),
                ]),
            ));
        }

        return Err(Error::validation_invalid_argument(
            "mode",
            "git workspace sync requires a clean working tree before remote execution",
            Some("git".to_string()),
            Some(vec![
                "Commit or stash local changes before git-backed Lab execution.".to_string(),
                "Run with --placement local to execute the command locally while the worktree is dirty."
                    .to_string(),
                "Use `homeboy runner workspace sync <runner-id> --path <local-worktree> --mode snapshot` when materializing a standalone snapshot workspace."
                    .to_string(),
            ]),
        ));
    }
    Ok(())
}

pub(super) struct GitMaterializationRequest<'a> {
    pub remote_path: &'a str,
    pub remote_url: &'a str,
    pub head: &'a str,
    pub branch: Option<&'a str>,
    pub changed_since_base: Option<&'a str>,
    pub git_fetch_refs: &'a [String],
    pub allow_dirty_lab_workspace: bool,
}

pub(super) fn materialize_git_controlled(
    runner: &Runner,
    request: GitMaterializationRequest<'_>,
    control: &WorkspaceControl,
) -> Result<()> {
    let command = materialize_git_command(
        request.remote_path,
        request.remote_url,
        request.head,
        request.branch,
        request.changed_since_base,
        request.git_fetch_refs,
        request.allow_dirty_lab_workspace,
    );
    control.shell(
        &super::util::shell_command_for_runner(runner, &command)?,
        "materialize git workspace",
    )
}

pub(super) struct ControllerGitBundleMaterializationRequest<'a> {
    pub local_path: &'a Path,
    pub remote_path: &'a str,
    pub head: &'a str,
    pub branch: Option<&'a str>,
    pub remote_url: &'a str,
    pub changed_since_base: Option<&'a str>,
    pub git_fetch_refs: &'a [String],
    pub allow_dirty_lab_workspace: bool,
}

pub(super) fn materialize_git_bundle_controlled(
    runner: &Runner,
    request: ControllerGitBundleMaterializationRequest<'_>,
    control: &WorkspaceControl,
) -> Result<ControllerGitBundleProvenance> {
    control.checkpoint()?;
    validate_controller_git_bundle_source(request.local_path, control)?;

    let bundle_dir = tempfile::tempdir().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("create controller git bundle directory".to_string()),
        )
    })?;
    let bundle_path = bundle_dir.path().join("workspace.bundle");

    // `HEAD` preserves the complete selected ancestry in a bundle. The exact
    // resolved SHA is recorded separately in provenance and verified on Lab.
    let refs = controller_bundle_refs("HEAD", request.changed_since_base, request.git_fetch_refs);
    hydrate_controller_bundle_objects_controlled(request.local_path, &refs, control)?;

    // Commits the runner's object cache already holds need not cross the wire.
    // A thin transfer that cannot be installed (the cache lost objects between
    // the query and the install) falls back to the complete closure, which
    // never depends on runner state.
    let object_cache = git_object_cache_path(request.remote_path, request.remote_url);
    let prerequisites = bundle_prerequisites(
        runner,
        request.local_path,
        request.head,
        &object_cache,
        control,
    )?;
    if !prerequisites.is_empty() {
        match transfer_git_bundle(
            runner,
            &request,
            &bundle_path,
            &refs,
            &prerequisites,
            &object_cache,
            control,
        ) {
            Ok(sha256) => {
                return Ok(controller_bundle_provenance(
                    &request,
                    refs,
                    prerequisites,
                    sha256,
                ))
            }
            Err(error) if control.checkpoint().is_err() => return Err(error),
            Err(_) => {}
        }
    }
    let sha256 = transfer_git_bundle(
        runner,
        &request,
        &bundle_path,
        &refs,
        &[],
        &object_cache,
        control,
    )?;
    Ok(controller_bundle_provenance(
        &request,
        refs,
        Vec::new(),
        sha256,
    ))
}

fn controller_bundle_provenance(
    request: &ControllerGitBundleMaterializationRequest<'_>,
    refs: Vec<String>,
    prerequisites: Vec<String>,
    sha256: String,
) -> ControllerGitBundleProvenance {
    ControllerGitBundleProvenance {
        provenance: "controller_git_bundle",
        source_sha: request.head.to_string(),
        source_refs: refs,
        prerequisites,
        sha256,
        cleanup_owner: "controller",
        // The controller tempdir is removed as soon as the transfer finishes.
        cleanup_ttl: "PT0S",
    }
}

/// Runner-side bare repository that accumulates every transferred closure for
/// one source remote. It lives beside the workspaces it seeds, so a later
/// transfer of the same repository only carries commits the runner lacks.
pub(crate) fn git_object_cache_path(remote_path: &str, remote_url: &str) -> String {
    let key = homeboy_engine_primitives::content_hash::sha256_hex(remote_url.as_bytes());
    format!(
        "{}/{GIT_OBJECT_CACHE_DIR}/{}.git",
        super::util::parent_remote_path(remote_path),
        &key[..16]
    )
}

const GIT_OBJECT_CACHE_DIR: &str = ".homeboy-git-cache";
/// Bounds both the controller query and the runner-side cache refs.
const GIT_OBJECT_CACHE_REF_LIMIT: usize = 64;

/// Commits in the runner cache that the controller can exclude from a bundle.
///
/// An exclusion must exist locally, and must not already contain `HEAD`:
/// excluding a descendant would leave nothing to bundle. Every other cached
/// commit is safe to exclude, ancestor or not, because Git bundles only the
/// objects unreachable from the exclusions and records the boundary commits as
/// prerequisites the cache is known to hold. An unreachable or empty cache
/// yields no exclusions and a complete transfer.
fn bundle_prerequisites(
    runner: &Runner,
    local_path: &Path,
    head: &str,
    object_cache: &str,
    control: &WorkspaceControl,
) -> Result<Vec<String>> {
    let list = format!(
        "git -C {cache} for-each-ref --sort=-committerdate --count={GIT_OBJECT_CACHE_REF_LIMIT} --format='%(objectname)' refs/homeboy/ 2>/dev/null || true",
        cache = shell::quote_arg(object_cache),
    );
    let output = control.output(
        Command::new("sh").args(["-c", &super::util::shell_command_for_runner(runner, &list)?]),
        "list runner git object cache",
    )?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let mut prerequisites = Vec::new();
    for commit in String::from_utf8_lossy(&output.stdout).lines() {
        let commit = commit.trim();
        if commit.len() < 40
            || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            || prerequisites.iter().any(|known| known == commit)
        {
            continue;
        }
        let present = control.output(
            Command::new("git")
                .args(["cat-file", "-e", &format!("{commit}^{{commit}}")])
                .env("GIT_NO_LAZY_FETCH", "1")
                .current_dir(local_path),
            "probe cached git commit",
        )?;
        if !present.status.success() {
            continue;
        }
        let contains_head = control.output(
            Command::new("git")
                .args(["merge-base", "--is-ancestor", head, commit])
                .current_dir(local_path),
            "probe cached git commit ancestry",
        )?;
        if !contains_head.status.success() {
            prerequisites.push(commit.to_string());
        }
    }
    Ok(prerequisites)
}

/// Commits named by the bundle's non-`HEAD` refs, which the runner workspace
/// must still contain once it stops borrowing from the object cache.
fn bundled_ref_commits(
    local_path: &Path,
    refs: &[String],
    control: &WorkspaceControl,
) -> Result<Vec<String>> {
    let mut commits = Vec::new();
    for git_ref in refs.iter().filter(|git_ref| git_ref.as_str() != "HEAD") {
        let commit = control.git(
            local_path,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("{git_ref}^{{commit}}"),
            ],
        )?;
        if !commits.contains(&commit) {
            commits.push(commit);
        }
    }
    Ok(commits)
}

/// Create one bundle, excluding `prerequisites`, and install it on the runner.
/// Returns the transferred bundle digest.
fn transfer_git_bundle(
    runner: &Runner,
    request: &ControllerGitBundleMaterializationRequest<'_>,
    bundle_path: &Path,
    refs: &[String],
    prerequisites: &[String],
    object_cache: &str,
    control: &WorkspaceControl,
) -> Result<String> {
    let output = control.output(
        Command::new("git")
            .arg("bundle")
            .arg("create")
            .arg(bundle_path)
            .args(refs)
            .args(prerequisites.iter().map(|commit| format!("^{commit}")))
            .env("GIT_NO_LAZY_FETCH", "1")
            .current_dir(request.local_path),
        "create git bundle",
    )?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "create git bundle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let sha256 = super::snapshot::snapshot_file_sha256(bundle_path, control)?;
    let retain = bundled_ref_commits(request.local_path, refs, control)?;

    let install_command = git_bundle_install_command(
        request.remote_path,
        request.head,
        request.branch,
        request.remote_url,
        request.changed_since_base,
        request.allow_dirty_lab_workspace,
        BundleTransfer {
            sha256: &sha256,
            object_cache,
            prerequisites,
            retain: &retain,
        },
    );
    match runner.kind {
        RunnerKind::Local => materialize_git_bundle_piped_controlled(
            bundle_path,
            &format!("sh -c {}", shell::quote_arg(&install_command)),
            "materialize local git bundle workspace",
            control,
        ),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                materialize_git_bundle_piped_controlled(
                    bundle_path,
                    &format!("sh -c {}", shell::quote_arg(&install_command)),
                    "materialize local git bundle workspace",
                    control,
                )
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let target = format!(
                    "ssh {ssh_args} {remote} {remote_command}",
                    ssh_args = ssh_args(&client),
                    remote = shell::quote_arg(&remote),
                    remote_command = shell::quote_arg(&install_command),
                );
                materialize_git_bundle_piped_controlled(
                    bundle_path,
                    &target,
                    "materialize SSH git bundle workspace",
                    control,
                )
            }
        }
    }?;
    Ok(sha256)
}

/// Materialize a controller Git workspace as its exact captured commit, then
/// apply its filtered working-tree contents. The controller completes and
/// transfers the object closure; a Lab must never contact the source remote.
pub(super) fn materialize_git_snapshot_controlled(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
    git_fetch_refs: &[String],
    control: &WorkspaceControl,
) -> Result<Option<ControllerGitBundleProvenance>> {
    control.checkpoint()?;
    let head = control.git(local_path, &["rev-parse", "HEAD"])?;
    let branch = control
        .git(local_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|branch| branch != "HEAD");
    let remote_url = control
        .git(local_path, &["config", "--get", "remote.origin.url"])
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| "homeboy-controller-bundle".to_string());
    let provenance = materialize_git_bundle_controlled(
        runner,
        ControllerGitBundleMaterializationRequest {
            local_path,
            remote_path,
            head: &head,
            branch: branch.as_deref(),
            remote_url: &remote_url,
            changed_since_base: None,
            git_fetch_refs,
            allow_dirty_lab_workspace: false,
        },
        control,
    )?;
    super::snapshot::materialize_snapshot_overlay_controlled(
        runner,
        local_path,
        remote_path,
        excludes,
        control,
    )?;
    control.checkpoint()?;
    verify_materialized_snapshot_git_representation(runner, remote_path)?;
    Ok(Some(provenance))
}

fn verify_materialized_snapshot_git_representation(
    runner: &Runner,
    remote_path: &str,
) -> Result<()> {
    match runner.kind {
        RunnerKind::Local => verify_valid_git_representation(Path::new(remote_path)),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                return verify_valid_git_representation(Path::new(remote_path));
            }
            let inside = super::snapshot::synthetic_checkout_value(
                runner,
                remote_path,
                "rev-parse --is-inside-work-tree",
            )?;
            if inside != "true" {
                return Err(Error::validation_invalid_argument(
                    "workspace",
                    "snapshot-git workspace .git representation is not a Git work tree",
                    Some(remote_path.to_string()),
                    None,
                ));
            }
            super::snapshot::synthetic_checkout_value(
                runner,
                remote_path,
                "rev-parse --verify -q HEAD",
            )
            .map(|_| ())
        }
    }
}

/// Resolve the exact object closure locally before `git bundle` can invoke a
/// promisor remote itself. The controller is the only participant allowed to
/// use the source checkout's authenticated transport.
#[cfg(test)]
pub(super) fn hydrate_controller_bundle_objects(local_path: &Path, refs: &[String]) -> Result<()> {
    hydrate_controller_bundle_objects_controlled(local_path, refs, &WorkspaceControl::default())
}

fn hydrate_controller_bundle_objects_controlled(
    local_path: &Path,
    refs: &[String],
    control: &WorkspaceControl,
) -> Result<()> {
    control.checkpoint()?;
    repair_controller_bundle_commit_closure(local_path, refs, control)?;
    if let Some(remote) = promisor_remote(local_path, control)? {
        return hydrate_promisor_bundle_objects(local_path, &remote, refs, control);
    }
    // Spool the complete closure in owned scratch, retaining the producer's
    // precise failure identity without an uncancellable pipe-copy or an
    // object-list-sized in-memory allocation.
    let objects = tempfile::NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("list git bundle objects".to_string()),
        )
    })?;
    let output = control.output_to_file(
        Command::new("git")
            .args([
                "-c",
                "core.commitGraph=false",
                "rev-list",
                "--objects",
                "--no-object-names",
            ])
            .args(refs)
            .current_dir(local_path),
        objects
            .reopen()
            .map_err(|error| Error::internal_io(error.to_string(), None))?,
        "list git bundle objects",
    )?;
    if !output.status.success() {
        return controller_bundle_failure(
            local_path,
            refs,
            "list git bundle objects",
            output.status.code(),
            control,
        );
    }
    let script = format!(
        "git -C {} cat-file --batch-check < {} >/dev/null",
        shell::quote_arg(&local_path.display().to_string()),
        shell::quote_arg(&objects.path().display().to_string())
    );
    let output = control.output(
        Command::new("bash").args(["-o", "pipefail", "-c", &script]),
        "hydrate git bundle objects",
    )?;
    if output.status.success() {
        Ok(())
    } else {
        controller_bundle_failure(
            local_path,
            refs,
            "hydrate git bundle objects",
            output.status.code(),
            control,
        )
    }
}

/// Hydrate a partial clone's bundle closure with one batched fetch.
///
/// Piping the closure into `git cat-file --batch-check` made Git lazy-fetch
/// each missing blob separately: one network round trip and one tiny promisor
/// pack per object. Repeated stagings of a blob:none checkout grew the
/// controller's store past 20,000 packs, which then made every later object
/// walk crawl. Instead, list the missing objects without lazy fetching and
/// fetch exactly those ids in one request, the same shape as Git's own
/// promisor fetch. One staging adds at most one pack, and the porcelain fetch
/// runs Git's automatic maintenance, which consolidates packs over time.
fn hydrate_promisor_bundle_objects(
    local_path: &Path,
    remote: &str,
    refs: &[String],
    control: &WorkspaceControl,
) -> Result<()> {
    let missing = all_missing_promisor_objects(local_path, refs, control)?;
    if !missing.is_empty() {
        if let Err(error) = fetch_promisor_objects(local_path, remote, &missing, control) {
            if error.details["workspace_sync"].is_object() {
                return Err(error);
            }
            return Err(controller_object_closure_error(
                "batched fetch of required promisor objects failed",
                error.details["git_exit_status"]
                    .as_i64()
                    .map(|value| value as i32),
                local_path,
                remote,
                refs,
                &missing[..missing.len().min(MISSING_PROMISOR_OBJECT_DIAGNOSTIC_LIMIT)],
            ));
        }
    }

    let still_missing = missing_promisor_objects(local_path, refs, control)?;
    if !still_missing.is_empty() {
        return Err(controller_object_closure_error(
            "hydrate git bundle objects completed with required promisor objects still unavailable",
            None,
            local_path,
            remote,
            refs,
            &still_missing,
        ));
    }
    Ok(())
}

/// Every object id in `refs`' closure that is absent locally, listed without
/// triggering lazy fetches. Unbounded, unlike the diagnostic probe.
fn all_missing_promisor_objects(
    local_path: &Path,
    refs: &[String],
    control: &WorkspaceControl,
) -> Result<Vec<String>> {
    let output = control.output_exact(
        Command::new("git")
            .args([
                "-c",
                "core.commitGraph=false",
                "rev-list",
                "--objects",
                "--no-object-names",
                "--missing=print",
            ])
            .args(refs)
            .env("GIT_NO_LAZY_FETCH", "1")
            .current_dir(local_path),
        "list missing git bundle objects",
    )?;
    if !output.status.success() {
        return Err(git_command_failure(
            "list missing git bundle objects",
            output.status.code(),
        ));
    }

    let mut seen = std::collections::HashSet::new();
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('?'))
        .filter(|object_id| seen.insert((*object_id).to_string()))
        .map(str::to_string)
        .collect())
}

/// Fetch explicit object ids from the promisor remote in one request.
/// Mirrors Git's own lazy-fetch invocation (noop negotiation, no tags, no
/// FETCH_HEAD) for the whole set at once. The transport's output stays out of
/// errors because it can carry credentials or remote details.
fn fetch_promisor_objects(
    local_path: &Path,
    remote: &str,
    object_ids: &[String],
    control: &WorkspaceControl,
) -> Result<()> {
    use std::io::Write as _;

    control.checkpoint()?;
    let mut input = tempfile::NamedTempFile::new()
        .map_err(|error| Error::internal_io(error.to_string(), None))?;
    for object in object_ids {
        control.checkpoint()?;
        writeln!(input, "{object}").map_err(|error| Error::internal_io(error.to_string(), None))?;
    }
    let args = [
        "-c",
        "fetch.negotiationAlgorithm=noop",
        "fetch",
        "--no-tags",
        "--no-write-fetch-head",
        "--recurse-submodules=no",
        "--filter=blob:none",
        "--stdin",
        remote,
    ];
    let mut process = Command::new("git");
    process.args(args).current_dir(local_path);
    homeboy_core::git::apply_configured_transport(&mut process, local_path, &args, &[]);
    let output = control.output_with_stdin(
        &mut process,
        input
            .reopen()
            .map_err(|error| Error::internal_io(error.to_string(), None))?,
        "fetch promisor objects",
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_command_failure(
            "fetch promisor objects",
            output.status.code(),
        ))
    }
}

fn repair_controller_bundle_commit_closure(
    local_path: &Path,
    refs: &[String],
    control: &WorkspaceControl,
) -> Result<()> {
    if let Some(remote) = promisor_remote(local_path, control)? {
        refetch_controller_bundle_commits(local_path, &remote, refs, control)?;
    }
    Ok(())
}

/// Rebuild the selected commit and tree closure before asking Git to walk it.
/// A commit graph can retain entries for promisor objects that are no longer in
/// the local object database, in which case `rev-list` cannot trigger its own
/// lazy fetch.
fn refetch_controller_bundle_commits(
    local_path: &Path,
    remote: &str,
    refs: &[String],
    control: &WorkspaceControl,
) -> Result<()> {
    // Only fetch refs whose local object closure is actually incomplete. A
    // changed-scope run selects the clean worktree HEAD alongside the resolved
    // base, but that HEAD can be a new *local-only* commit that was never pushed
    // (#8309). The promisor remote cannot serve it — `git fetch <remote> <sha>`
    // fails with "upload-pack: not our ref <sha>" — and it does not need to,
    // because a locally-authored commit is already fully present. Fetching only
    // the refs with missing objects hydrates the promised base/ancestry closure
    // through the promisor transport while leaving fully-local commits alone.
    let mut incomplete_refs = Vec::new();
    for git_ref in refs {
        control.checkpoint()?;
        if !missing_promisor_objects(local_path, &[git_ref.clone()], control)?.is_empty() {
            incomplete_refs.push(git_ref.clone());
        }
    }

    if incomplete_refs.is_empty() {
        return Ok(());
    }

    // Fetch only the closure of the requested refs. `--refetch` was used here to
    // reapply the partial-clone filter, but it "fetches all objects as a fresh
    // clone would" (git-fetch(1)), re-pulling objects reachable from unrelated
    // refs the server happens to pack alongside the requested ones. That
    // over-fetch is non-deterministic and violated the changed-scope contract
    // that the controller must hydrate only the promised base/head closure.
    // Naming the exact refs completes their closure through the promisor
    // transport without re-fetching unrelated history.
    let mut args = vec!["fetch", "--no-tags", "--filter=blob:none", remote];
    args.extend(incomplete_refs.iter().map(String::as_str));
    let mut process = Command::new("git");
    process.args(&args).current_dir(local_path);
    homeboy_core::git::apply_configured_transport(&mut process, local_path, &args, &[]);
    let output = control.output(&mut process, "refetch controller git bundle commits")?;
    if output.status.success() {
        return Ok(());
    }

    // `incomplete_refs` was derived from explicit missing-object probe output
    // in a promisor-configured checkout. The transport result itself stays out
    // of provenance: it may contain credentials or remote implementation data.
    let missing = missing_promisor_objects(local_path, refs, control).unwrap_or_default();
    control.checkpoint()?;
    Err(controller_object_closure_error(
        "refetch controller git bundle commits failed while required promisor objects remained unavailable",
        output.status.code(),
        local_path,
        remote,
        refs,
        &missing,
    ))
}

fn controller_bundle_failure(
    local_path: &Path,
    refs: &[String],
    action: &str,
    exit_status: Option<i32>,
    control: &WorkspaceControl,
) -> Result<()> {
    match missing_promisor_objects(local_path, refs, control).map(|missing| !missing.is_empty()) {
        Ok(true) => {
            let remote = promisor_remote(local_path, control)?.unwrap_or_default();
            let missing = missing_promisor_objects(local_path, refs, control)?;
            Err(controller_object_closure_error(
                format!("{action} failed while required promisor objects remained unavailable"),
                exit_status,
                local_path,
                &remote,
                refs,
                &missing,
            ))
        }
        // Preserve the operation that actually failed. A probe failure is not
        // evidence that this checkout has a missing promisor-object closure.
        Ok(false) | Err(_) => Err(git_command_failure(action, exit_status)),
    }
}

fn controller_object_closure_error(
    message: impl Into<String>,
    exit_status: Option<i32>,
    local_path: &Path,
    remote: &str,
    refs: &[String],
    missing: &[String],
) -> Error {
    let repair_command = format!(
        "git -C {} fetch --no-tags --filter=blob:none {} {}",
        shell::quote_arg(&local_path.display().to_string()),
        shell::quote_arg(remote),
        refs.iter()
            .map(|git_ref| shell::quote_arg(git_ref))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let mut error = Error::internal_unexpected(format!(
        "{}; repair the controller checkout with `{repair_command}`",
        message.into()
    ));
    error.details = serde_json::json!({
        "reason": "controller_git_object_closure_unavailable",
        "git_exit_status": exit_status,
        "missing_object_ids": missing,
        "repair_command": repair_command,
    });
    error
}

fn git_command_failure(action: &str, exit_status: Option<i32>) -> Error {
    let mut error = Error::internal_unexpected(format!(
        "{action} failed while resolving the controller object closure"
    ));
    error.details = serde_json::json!({ "git_exit_status": exit_status });
    error
}

/// Report whether `git_ref`'s local object closure is missing any objects.
///
/// Walks the ref with lazy fetching disabled so a partial clone *reports* absent
/// promisor objects instead of silently fetching them. A ref that is fully
/// present locally (e.g. a new local-only commit that was never pushed) reports
/// no missing objects and must not be requested from the promisor remote, which
/// cannot serve an unpushed commit. A ref that cannot be resolved at all (its
/// tip commit is itself absent) is treated as incomplete so the promisor fetch
/// can attempt to hydrate it.
#[cfg(test)]
pub(super) fn ref_has_missing_objects(local_path: &Path, git_ref: &str) -> Result<bool> {
    Ok(!missing_promisor_objects(
        local_path,
        &[git_ref.to_string()],
        &WorkspaceControl::default(),
    )?
    .is_empty())
}

const MISSING_PROMISOR_OBJECT_DIAGNOSTIC_LIMIT: usize = 8;

/// Return a bounded list of missing promised objects without allowing Git to
/// hydrate them. The list is diagnostic only; closure hydration remains the
/// controller's responsibility.
fn missing_promisor_objects(
    local_path: &Path,
    refs: &[String],
    control: &WorkspaceControl,
) -> Result<Vec<String>> {
    if promisor_remote(local_path, control)?.is_none() {
        return Ok(Vec::new());
    }

    let mut missing = Vec::new();
    for git_ref in refs {
        let output = control.output_exact(
            Command::new("git")
                .args([
                    "-c",
                    "core.commitGraph=false",
                    "rev-list",
                    "--objects",
                    "--no-object-names",
                    "--missing=print",
                ])
                .arg(git_ref)
                .env("GIT_NO_LAZY_FETCH", "1")
                .current_dir(local_path),
            "probe controller bundle ref objects",
        )?;

        if !output.status.success() {
            return Err(git_command_failure(
                "probe controller bundle ref objects",
                output.status.code(),
            ));
        }

        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Some(object_id) = line.strip_prefix('?') else {
                continue;
            };
            if missing.len() == MISSING_PROMISOR_OBJECT_DIAGNOSTIC_LIMIT {
                return Ok(missing);
            }
            missing.push(object_id.to_string());
        }
    }

    Ok(missing)
}

fn promisor_remote(local_path: &Path, control: &WorkspaceControl) -> Result<Option<String>> {
    let output = control.output_exact(
        Command::new("git")
            .args(["config", "--get-regexp", r"^remote\..*\.promisor$"])
            .current_dir(local_path),
        "read git promisor remote",
    )?;
    if !output.status.success() {
        return Ok(None);
    }

    let remote = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(char::is_whitespace)?;
            if value.trim() != "true" {
                return None;
            }
            key.strip_prefix("remote.")?.strip_suffix(".promisor")
        })
        .map(str::to_string);
    Ok(remote)
}

fn controller_bundle_refs(
    head: &str,
    changed_since_base: Option<&str>,
    git_fetch_refs: &[String],
) -> Vec<String> {
    let mut refs = vec![head.to_string()];
    if let Some(base) = changed_since_base {
        push_unique_bundle_ref(&mut refs, base);
    }
    for git_ref in git_fetch_refs {
        // A fetch refspec may name a destination for runner-side fetches. A
        // bundle only needs its controller-local source ref.
        let source_ref = git_ref
            .trim_start_matches('+')
            .split_once(':')
            .map_or(git_ref.as_str(), |(source, _)| source);
        push_unique_bundle_ref(&mut refs, source_ref);
    }
    refs
}

fn push_unique_bundle_ref(refs: &mut Vec<String>, git_ref: &str) {
    if !git_ref.trim().is_empty() && !refs.iter().any(|existing| existing == git_ref) {
        refs.push(git_ref.to_string());
    }
}

fn validate_controller_git_bundle_source(
    local_path: &Path,
    control: &WorkspaceControl,
) -> Result<()> {
    let is_shallow = control.git(local_path, &["rev-parse", "--is-shallow-repository"])?;
    if is_shallow.trim() != "true" {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "path",
        "controller-routed git workspace sync requires a full source checkout before creating a runner git bundle; the selected source checkout is shallow",
        Some(local_path.display().to_string()),
        Some(vec![
            format!(
                "Deepen the source checkout with `git -C {} fetch --unshallow` before retrying.",
                shell::quote_arg(&local_path.display().to_string())
            ),
            "Use a full clone for --source-path when upgrading runners with --method source."
                .to_string(),
            "Use snapshot workspace sync only when the remote command does not need Git history."
                .to_string(),
        ]),
    ))
}

fn materialize_git_bundle_piped_controlled(
    bundle_path: &Path,
    target_command: &str,
    action: &str,
    control: &WorkspaceControl,
) -> Result<()> {
    let command = format!(
        "cat {bundle} | {target_command}",
        bundle = shell::quote_arg(&bundle_path.display().to_string()),
        target_command = target_command,
    );
    control.shell(&command, action)
}

/// How one bundle relates to the runner's object cache.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BundleTransfer<'a> {
    /// Digest the runner verifies before Git reads the transfer.
    pub sha256: &'a str,
    pub object_cache: &'a str,
    /// Cached commits excluded from the bundle; empty for a complete closure.
    pub prerequisites: &'a [String],
    /// Bundled non-`HEAD` commits the workspace must keep after it stops
    /// borrowing from the cache.
    pub retain: &'a [String],
}

pub(crate) fn git_bundle_install_command(
    remote_path: &str,
    head: &str,
    branch: Option<&str>,
    remote_url: &str,
    changed_since_base: Option<&str>,
    allow_dirty_lab_workspace: bool,
    transfer: BundleTransfer<'_>,
) -> String {
    WorkspaceMaterializer::new(remote_path)
        .with_bundle_file(transfer.object_cache)
        .capture_owner()
        .op(WorkspaceMaterializationOperation::EnsureParent)
        .op(WorkspaceMaterializationOperation::CleanupOnExit(vec![
            "\"$tmp\"".to_string(),
            "\"$bundle\"".to_string(),
        ]))
        .op(WorkspaceMaterializationOperation::WriteStdinToBundle)
        .op(WorkspaceMaterializationOperation::VerifyBundleDigest(
            transfer.sha256.to_string(),
        ))
        .op(
            WorkspaceMaterializationOperation::RecordBundleInObjectCache {
                head: head.to_string(),
                ref_limit: GIT_OBJECT_CACHE_REF_LIMIT,
            },
        )
        .op(WorkspaceMaterializationOperation::CloneBundleToTemp {
            borrow_cache: !transfer.prerequisites.is_empty(),
            retain: transfer.retain.to_vec(),
        })
        .op(WorkspaceMaterializationOperation::SetGitOrigin(
            remote_url.to_string(),
        ))
        .op(WorkspaceMaterializationOperation::CheckoutGitRef {
            head: head.to_string(),
            branch: branch.map(str::to_string),
        })
        .op(WorkspaceMaterializationOperation::ResetAndCleanGit {
            head: head.to_string(),
        })
        .op(WorkspaceMaterializationOperation::GuardCleanGitWorkspace {
            allow_dirty: allow_dirty_lab_workspace,
        })
        .op(WorkspaceMaterializationOperation::AtomicReplaceTemp)
        .op(WorkspaceMaterializationOperation::VerifyGitBaseline {
            remote_url: remote_url.to_string(),
            head: head.to_string(),
            changed_since_base: changed_since_base.map(str::to_string),
        })
        .restore_owner()
        .command()
}

pub(super) fn materialize_git_command(
    remote_path: &str,
    remote_url: &str,
    head: &str,
    branch: Option<&str>,
    changed_since_base: Option<&str>,
    git_fetch_refs: &[String],
    allow_dirty_lab_workspace: bool,
) -> String {
    WorkspaceMaterializer::new(remote_path)
        .capture_owner()
        .op(WorkspaceMaterializationOperation::EnsureParent)
        .op(WorkspaceMaterializationOperation::SyncGitCheckout {
            remote_url: remote_url.to_string(),
            head: head.to_string(),
            branch: branch.map(str::to_string),
            changed_since_base: changed_since_base.map(str::to_string),
            fetch_refs: git_fetch_refs.to_vec(),
            allow_dirty: allow_dirty_lab_workspace,
        })
        .op(WorkspaceMaterializationOperation::VerifyGitBaseline {
            remote_url: remote_url.to_string(),
            head: head.to_string(),
            changed_since_base: changed_since_base.map(str::to_string),
        })
        .restore_owner()
        .command()
}
