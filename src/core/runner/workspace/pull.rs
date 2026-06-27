use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::core::error::{Error, Result};
use crate::core::server;

use super::super::{load, Runner, RunnerKind};
use super::types::{
    ByteFileCounts, RunnerWorkspacePullOptions, RunnerWorkspacePullOutput, RunnerWorkspacePullPlan,
};

pub fn pull_workspace(
    runner_id: &str,
    options: RunnerWorkspacePullOptions,
) -> Result<(RunnerWorkspacePullOutput, i32)> {
    let runner = load(runner_id)?;
    let plan = plan_workspace_pull(&runner, &options)?;

    if !options.dry_run {
        fs::create_dir_all(&plan.local_destination).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("create {}", plan.local_destination)),
            )
        })?;
    }
    let before = if options.dry_run {
        BTreeMap::new()
    } else {
        local_file_stats(Path::new(&plan.local_destination))?
    };

    if !options.dry_run {
        match runner.kind {
            RunnerKind::Local => pull_local(&plan)?,
            RunnerKind::Ssh => pull_ssh(&runner, &plan)?,
        }
    }

    let counts = if options.dry_run {
        ByteFileCounts::default()
    } else {
        changed_local_file_counts(Path::new(&plan.local_destination), &before)?
    };

    Ok((
        RunnerWorkspacePullOutput {
            variant: "workspace_pull",
            command: "runner.workspace.pull",
            runner_id: plan.runner_id,
            remote_path: plan.remote_path,
            includes: plan.includes,
            local_destination: plan.local_destination,
            remote_sources: plan.remote_sources,
            allowed_roots: plan.allowed_roots,
            dry_run: options.dry_run,
            files: counts.files,
            bytes: counts.bytes,
        },
        0,
    ))
}

pub fn plan_workspace_pull(
    runner: &Runner,
    options: &RunnerWorkspacePullOptions,
) -> Result<RunnerWorkspacePullPlan> {
    let remote_path = normalize_remote_path(&options.remote_path)?;
    let includes = normalize_includes(&options.includes)?;
    let local_destination = normalize_local_destination(&options.to)?;
    let allowed_roots = allowed_runner_workspace_roots(runner)?;

    if !allowed_roots
        .iter()
        .any(|root| path_is_within_root(&remote_path, root))
    {
        return Err(Error::validation_invalid_argument(
            "remote_path",
            "runner workspace pull path must be under an allowed runner workspace root",
            Some(remote_path),
            Some(vec![format!("Allowed roots: {}", allowed_roots.join(", "))]),
        ));
    }

    let remote_sources = includes
        .iter()
        .map(|include| join_remote_path(&remote_path, include))
        .collect();

    Ok(RunnerWorkspacePullPlan {
        runner_id: runner.id.clone(),
        remote_path,
        includes,
        local_destination,
        remote_sources,
        allowed_roots,
    })
}

fn pull_local(plan: &RunnerWorkspacePullPlan) -> Result<()> {
    for include in &plan.includes {
        let pattern = join_remote_path(&plan.remote_path, include);
        let entries = glob::glob(&pattern).map_err(|err| {
            Error::validation_invalid_argument(
                "include",
                format!("invalid include glob: {err}"),
                Some(include.clone()),
                None,
            )
        })?;
        for entry in entries {
            let source = entry.map_err(|err| {
                Error::internal_io(err.to_string(), Some(format!("glob {pattern}")))
            })?;
            if source.is_file() {
                let relative = source.strip_prefix(&plan.remote_path).unwrap_or(&source);
                let destination = Path::new(&plan.local_destination).join(relative);
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).map_err(|err| {
                        Error::internal_io(
                            err.to_string(),
                            Some(format!("create {}", parent.display())),
                        )
                    })?;
                }
                fs::copy(&source, &destination).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some(format!(
                            "copy {} to {}",
                            source.display(),
                            destination.display()
                        )),
                    )
                })?;
            }
        }
    }
    Ok(())
}

fn pull_ssh(runner: &Runner, plan: &RunnerWorkspacePullPlan) -> Result<()> {
    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner",
            "SSH runner is missing server_id",
            Some(runner.id.clone()),
            None,
        )
    })?;

    for source in &plan.remote_sources {
        let output = server::transfer::transfer(&server::transfer::TransferConfig {
            source: format!("{server_id}:{source}"),
            destination: plan.local_destination.clone(),
            recursive: true,
            compress: true,
            dry_run: false,
            exclude: Vec::new(),
        })?;
        if output.1 != 0 || !output.0.success {
            return Err(Error::validation_invalid_argument(
                "remote_path",
                format!(
                    "failed to pull runner workspace files: {}",
                    output.0.error.unwrap_or_else(|| "scp failed".to_string())
                ),
                Some(source.clone()),
                None,
            ));
        }
    }
    Ok(())
}

fn normalize_remote_path(path: &str) -> Result<String> {
    let path = path.trim_end_matches('/');
    let parsed = Path::new(path);
    if !parsed.is_absolute() {
        return Err(Error::validation_invalid_argument(
            "remote_path",
            "runner workspace pull path must be absolute",
            Some(path.to_string()),
            None,
        ));
    }
    if has_parent_dir(parsed) {
        return Err(Error::validation_invalid_argument(
            "remote_path",
            "runner workspace pull path must not contain parent directory components",
            Some(path.to_string()),
            None,
        ));
    }
    Ok(if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    })
}

fn normalize_includes(includes: &[String]) -> Result<Vec<String>> {
    let mut normalized = if includes.is_empty() {
        vec!["**/*".to_string()]
    } else {
        includes.to_vec()
    };
    for include in &mut normalized {
        *include = include.trim_start_matches('/').to_string();
        let path = Path::new(include);
        if include.is_empty() || path.is_absolute() || has_parent_dir(path) {
            return Err(Error::validation_invalid_argument(
                "include",
                "include glob must be a non-empty relative path without parent directory components",
                Some(include.clone()),
                None,
            ));
        }
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn normalize_local_destination(path: &str) -> Result<String> {
    let expanded = shellexpand::tilde(path).to_string();
    if expanded.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "to",
            "local destination must not be empty",
            None,
            None,
        ));
    }
    Ok(expanded)
}

fn allowed_runner_workspace_roots(runner: &Runner) -> Result<Vec<String>> {
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
    roots.sort();
    roots.dedup();
    roots.retain(|root| Path::new(root).is_absolute());
    if roots.is_empty() {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner has no configured workspace roots for workspace pull",
            Some(runner.id.clone()),
            Some(vec![
                "Set runner.workspace_root or policy.workspace_roots before pulling workspace files."
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

fn join_remote_path(base: &str, include: &str) -> String {
    if base == "/" {
        format!("/{include}")
    } else {
        format!("{base}/{include}")
    }
}

fn has_parent_dir(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn local_file_stats(
    root: &Path,
) -> Result<BTreeMap<PathBuf, (u64, Option<std::time::SystemTime>)>> {
    let mut stats = BTreeMap::new();
    collect_local_file_stats(root, root, &mut stats)?;
    Ok(stats)
}

fn collect_local_file_stats(
    root: &Path,
    path: &Path,
    stats: &mut BTreeMap<PathBuf, (u64, Option<std::time::SystemTime>)>,
) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("read {}", path.display())))
    })? {
        let entry = entry.map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("read {}", path.display())))
        })?;
        let child = entry.path();
        let metadata = entry.metadata().map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("stat {}", child.display())))
        })?;
        if metadata.is_dir() {
            collect_local_file_stats(root, &child, stats)?;
        } else if metadata.is_file() {
            let relative = child.strip_prefix(root).unwrap_or(&child).to_path_buf();
            stats.insert(relative, (metadata.len(), metadata.modified().ok()));
        }
    }
    Ok(())
}

fn changed_local_file_counts(
    root: &Path,
    before: &BTreeMap<PathBuf, (u64, Option<std::time::SystemTime>)>,
) -> Result<ByteFileCounts> {
    let after = local_file_stats(root)?;
    let mut counts = ByteFileCounts::default();
    for (path, stat) in after {
        if before.get(&path) != Some(&stat) {
            counts.files += 1;
            counts.bytes += stat.0;
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::core::runner::{Runner, RunnerKind};
    use crate::core::server::{RunnerPolicy, RunnerSettings};

    use super::*;

    fn runner() -> Runner {
        Runner {
            id: "lab".to_string(),
            kind: RunnerKind::Ssh,
            server_id: Some("lab-server".to_string()),
            workspace_root: Some("/srv/homeboy".to_string()),
            settings: RunnerSettings::default(),
            env: HashMap::new(),
            secret_env: HashMap::new(),
            resources: HashMap::new(),
            policy: RunnerPolicy::default(),
        }
    }

    #[test]
    fn pull_plan_accepts_path_under_workspace_root() {
        let plan = plan_workspace_pull(
            &runner(),
            &RunnerWorkspacePullOptions {
                remote_path: "/srv/homeboy/snapshots/run-1".to_string(),
                includes: vec!["fixtures/*.fig".to_string()],
                to: "./fixtures".to_string(),
                dry_run: true,
            },
        )
        .expect("plan");

        assert_eq!(plan.runner_id, "lab");
        assert_eq!(
            plan.remote_sources,
            vec!["/srv/homeboy/snapshots/run-1/fixtures/*.fig"]
        );
        assert_eq!(plan.allowed_roots, vec!["/srv/homeboy"]);
    }

    #[test]
    fn pull_plan_rejects_path_outside_workspace_root() {
        let err = plan_workspace_pull(
            &runner(),
            &RunnerWorkspacePullOptions {
                remote_path: "/etc".to_string(),
                includes: vec!["*.conf".to_string()],
                to: ".".to_string(),
                dry_run: true,
            },
        )
        .expect_err("outside root should fail");

        assert!(err.to_string().contains("allowed runner workspace root"));
    }

    #[test]
    fn pull_plan_rejects_parent_components() {
        let err = plan_workspace_pull(
            &runner(),
            &RunnerWorkspacePullOptions {
                remote_path: "/srv/homeboy/../secret".to_string(),
                includes: vec!["*.fig".to_string()],
                to: ".".to_string(),
                dry_run: true,
            },
        )
        .expect_err("parent remote path should fail");
        assert!(err.to_string().contains("parent directory"));

        let err = plan_workspace_pull(
            &runner(),
            &RunnerWorkspacePullOptions {
                remote_path: "/srv/homeboy/work".to_string(),
                includes: vec!["../*.fig".to_string()],
                to: ".".to_string(),
                dry_run: true,
            },
        )
        .expect_err("parent include should fail");
        assert!(err.to_string().contains("relative path"));
    }
}
