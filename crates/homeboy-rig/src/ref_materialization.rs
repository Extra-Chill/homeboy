//! Immutable ref checkouts shared by local rig runs and Lab dependency snapshots.
use std::collections::BTreeMap;
use std::path::Path;

use homeboy_core::git::{normalize_pr_ref, run_git};
use homeboy_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::{component_ref, resolve_component_path, RigSpec};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentRefMaterialization {
    pub reference: String,
    pub resolved_ref: String,
    pub pr: Option<String>,
    pub repository: String,
    pub head_repository: Option<String>,
    pub head_sha: String,
    pub worktree_path: String,
}

/// Fetch PR heads from the base repository's pull ref, including fork PRs.
/// Fetching before resolving makes mutable branches and PRs fresh on every run.
pub fn materialize_ref(checkout: &Path, reference: &str) -> Result<ComponentRefMaterialization> {
    let reference = reference.trim();
    if reference.is_empty() || reference.starts_with('-') {
        return Err(invalid("ref must be a PR, branch, or commit"));
    }
    // Serialize fetch/FETCH_HEAD and worktree creation across rig invocations.
    let common = run_git(
        checkout,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        "resolve repository",
    )?;
    let roots = homeboy_core::paths::PathRoots::from_environment()?;
    let managed_root = roots.config().join("worktrees").join("rig-refs");
    std::fs::create_dir_all(&managed_root).map_err(|error| invalid(&error.to_string()))?;
    let lock_key = homeboy_engine_primitives::content_hash::sha256_hex(common.trim().as_bytes());
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(managed_root.join(format!("{lock_key}.lock")))
        .map_err(|error| invalid(&error.to_string()))?;
    fs4::fs_std::FileExt::lock_exclusive(&lock).map_err(|error| invalid(&error.to_string()))?;
    let origin = run_git(checkout, &["remote", "get-url", "origin"], "resolve origin").ok();
    let local_repository = checkout.to_string_lossy();
    let origin = origin
        .as_deref()
        .map(str::trim)
        .unwrap_or(&local_repository);
    let full_sha = reference.len() == 40 && reference.bytes().all(|b| b.is_ascii_hexdigit());
    let pr = if (!full_sha && reference.bytes().all(|b| b.is_ascii_digit()))
        || (reference.contains("://") && reference.contains("/pull/"))
    {
        Some(normalize_pr_ref(reference)?)
    } else {
        reference
            .strip_prefix("refs/pull/")
            .and_then(|value| value.strip_suffix("/head"))
            .map(str::to_owned)
    };
    if pr
        .as_deref()
        .is_some_and(|number| number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(invalid("invalid PR number"));
    }
    let repository = if reference.contains("://") && pr.is_some() {
        reference.split("/pull/").next().unwrap_or(origin)
    } else {
        origin
    };
    let pr_head = if let (Some(number), Some(repo)) = (
        pr.as_deref(),
        homeboy_core::git::release_download::parse_github_url(repository),
    ) {
        let client = homeboy_core::git::GhClient::for_repo(&repo);
        Some(
            client
                .api_json::<PullHead>(&format!(
                    "repos/{}/{}/pulls/{number}",
                    repo.owner, repo.repo
                ))?
                .head,
        )
    } else {
        None
    };
    let mut resolved_ref = pr
        .as_ref()
        .map(|number| format!("refs/pull/{number}/head"))
        .unwrap_or_else(|| {
            reference
                .strip_prefix("origin/")
                .unwrap_or(reference)
                .to_string()
        });
    if full_sha {
        let refs = if origin != local_repository {
            crate::origin_refs::advertised_origin_refs_for_commit(
                checkout,
                reference,
                "ref",
                "could not resolve origin ref",
                reference.to_string(),
                Vec::new(),
            )?
        } else {
            Vec::new()
        };
        resolved_ref = crate::origin_refs::best_advertised_ref(refs).unwrap_or(resolved_ref);
    }
    run_git(
        checkout,
        &["fetch", "--no-tags", "--", repository, &resolved_ref],
        "fetch rig ref",
    )?;
    let head_sha = run_git(
        checkout,
        &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
        "resolve rig head",
    )?
    .trim()
    .to_string();
    if full_sha && !head_sha.eq_ignore_ascii_case(reference) {
        return Err(invalid("advertised ref moved while resolving pinned SHA"));
    }
    if pr_head.as_ref().is_some_and(|head| head.sha != head_sha) {
        return Err(invalid(
            "PR head moved during resolution; retry to materialize the new head",
        ));
    }
    // Include the owning repository's common directory: worktrees belong to
    // that repository, not another clone with the same origin URL.
    let repository_key = homeboy_core::git::release_download::parse_github_url(repository)
        .map(|repo| format!("{}/{}/{}", repo.host, repo.owner, repo.repo).to_ascii_lowercase())
        .unwrap_or_else(|| {
            repository
                .trim_end_matches('/')
                .trim_end_matches(".git")
                .to_string()
        });
    let identity = format!("{}\n{}", common.trim(), repository_key);
    let key = homeboy_engine_primitives::content_hash::sha256_hex(identity.as_bytes());
    let parent = managed_root.join(key);
    std::fs::create_dir_all(&parent).map_err(|error| invalid(&error.to_string()))?;
    let path = parent.join(&head_sha);
    let path_string = path.to_string_lossy().to_string();
    if !path.exists() {
        run_git(
            checkout,
            &["worktree", "add", "--detach", &path_string, &head_sha],
            "materialize rig ref",
        )?;
    }
    let actual = run_git(&path, &["rev-parse", "HEAD"], "verify rig worktree")?;
    let branch = run_git(
        &path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "verify detached rig worktree",
    )?;
    let worktree_common = run_git(
        &path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        "verify worktree repository",
    )?;
    if actual.trim() != head_sha
        || branch.trim() != "HEAD"
        || worktree_common.trim() != common.trim()
    {
        return Err(invalid("managed rig worktree no longer matches its detached head; remove it with git worktree remove before retrying"));
    }
    Ok(ComponentRefMaterialization {
        reference: reference.to_string(),
        resolved_ref,
        pr,
        repository: repository.to_string(),
        head_repository: pr_head.and_then(|head| head.repo.map(|repo| repo.clone_url)),
        head_sha,
        worktree_path: path_string,
    })
}

/// Return an invocation-local spec. Never mutate environment overrides or the
/// installed rig; all pipeline and service expansion uses this effective spec.
pub fn materialize_components(
    rig: &RigSpec,
    overrides: &BTreeMap<String, String>,
) -> Result<(RigSpec, BTreeMap<String, ComponentRefMaterialization>)> {
    for id in overrides.keys() {
        if !rig.components.contains_key(id) {
            return Err(invalid(&format!(
                "component '{id}' is not declared in rig '{}'",
                rig.id
            )));
        }
    }
    let mut effective = rig.clone();
    let mut records = BTreeMap::new();
    for (id, spec) in &rig.components {
        let Some(reference) = overrides.get(id).cloned().or_else(|| component_ref(spec)) else {
            continue;
        };
        let checkout = resolve_component_path(rig, id)?;
        let checkout = Path::new(&checkout);
        let prefix = run_git(
            checkout,
            &["rev-parse", "--show-prefix"],
            "resolve component subpath",
        )?;
        let record = materialize_ref(checkout, &reference)?;
        let component_path = if prefix.trim().is_empty() {
            std::path::PathBuf::from(&record.worktree_path)
        } else {
            Path::new(&record.worktree_path).join(prefix.trim())
        };
        effective
            .effective_component_paths
            .insert(id.clone(), component_path.to_string_lossy().to_string());
        records.insert(id.clone(), record);
    }
    Ok((effective, records))
}

#[derive(Deserialize)]
struct PullHead {
    head: PullHeadIdentity,
}

#[derive(Deserialize)]
struct PullHeadIdentity {
    sha: String,
    repo: Option<PullRepository>,
}

#[derive(Deserialize)]
struct PullRepository {
    clone_url: String,
}

fn invalid(message: &str) -> Error {
    Error::validation_invalid_argument("ref", message, None, None)
}
