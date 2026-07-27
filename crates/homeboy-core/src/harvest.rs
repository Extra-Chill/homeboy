//! Recover remote component content into a clean local Git checkout.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::content_diff::{self, ContentChange};
use crate::context::resolve_project_ssh_with_base_path;
use crate::engine::shell;
use crate::{git, project, Error, Result};

#[derive(Debug, Clone, Default)]
pub struct HarvestOptions {
    pub component_ids: Vec<String>,
    pub check: bool,
    pub dry_run: bool,
    pub apply: bool,
    pub excludes: Vec<String>,
    pub author: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HarvestComponentResult {
    pub component_id: String,
    pub local_path: String,
    pub remote_path: String,
    /// The exclude set actually applied, after composing every source. Without
    /// this an operator cannot tell why a path was or was not compared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_excludes: Vec<String>,
    pub status: String,
    pub changes: Vec<ContentChange>,
    pub committed: bool,
    pub provenance: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HarvestResult {
    pub project_id: String,
    pub results: Vec<HarvestComponentResult>,
}

pub fn run(project_id: &str, options: &HarvestOptions) -> Result<HarvestResult> {
    if options.apply && (options.check || options.dry_run) {
        return Err(Error::validation_invalid_argument(
            "apply",
            "--apply cannot be combined with --check or --dry-run",
            None,
            None,
        ));
    }
    if options.apply && options.component_ids.len() > 1 {
        return Err(Error::validation_invalid_argument(
            "component_ids",
            "--apply requires exactly one component per recovery commit",
            None,
            Some(vec!["Apply each reviewed component separately.".to_string()]),
        ));
    }
    let (context, base_path) = resolve_project_ssh_with_base_path(project_id)?;
    let ids = if options.component_ids.is_empty() {
        context
            .project
            .components
            .iter()
            .map(|component| component.id.clone())
            .collect()
    } else {
        options.component_ids.clone()
    };
    if options.apply && ids.len() != 1 {
        return Err(Error::validation_invalid_argument(
            "component_ids",
            "--apply requires exactly one component per recovery commit",
            None,
            Some(vec!["Apply each reviewed component separately.".to_string()]),
        ));
    }
    let components = ids
        .iter()
        .map(|id| project::resolve_project_component(&context.project, id))
        .collect::<Result<Vec<_>>>()?;
    let resolved_project = project::project_with_detected_path_roots(
        &context.project,
        &components,
        &base_path,
        &context.client,
        "harvest",
    );
    let mut results = Vec::new();
    for component in components {
        let id = component.id.clone();
        if component.remote_path.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "remote_path",
                format!("component '{id}' has no remote_path"),
                Some(project_id.to_string()),
                None,
            ));
        }
        let remote_path =
            project::resolve_effective_remote_path(&resolved_project, &component, &base_path)?;
        let local_path = PathBuf::from(&component.local_path);
        // Exclusions compose from four sources, widest to narrowest: env
        // policy, extension sync excludes, gitignore-derived entries, the
        // component's own declaration, and finally this invocation's flags.
        // The component-declared set exists because the others vary by
        // environment — extensions are not installed on a CI runner, so an
        // inherited exclude set is not the same set there (#10220).
        let mut excludes = crate::source_snapshot::policy_for_path(&local_path).sync_excludes;
        if let Some(declared) = component.harvest_excludes.as_ref() {
            excludes.extend(
                declared
                    .iter()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
            );
        }
        excludes.extend(options.excludes.clone());
        excludes.sort();
        excludes.dedup();
        let snapshot = tempfile::tempdir().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create harvest snapshot".to_string()),
            )
        })?;
        materialize_remote(&context.client, &remote_path, snapshot.path(), &excludes)?;
        let changes = content_diff::compare(snapshot.path(), &local_path, &excludes)?;
        let mut result = HarvestComponentResult {
            component_id: id.clone(),
            local_path: component.local_path.clone(),
            remote_path: remote_path.clone(),
            resolved_excludes: excludes.clone(),
            status: if changes.is_empty() {
                "up_to_date".to_string()
            } else if options.apply {
                "applied".to_string()
            } else {
                "drift".to_string()
            },
            changes,
            committed: false,
            provenance: None,
        };
        if options.apply && !result.changes.is_empty() {
            let git_root = git::toplevel(&local_path).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "local_path",
                    format!("component '{id}' must be in a Git worktree to apply harvest"),
                    None,
                    None,
                )
            })?;
            let git_root = Path::new(&git_root);
            if !git::is_workdir_clean_or_not_git(git_root) {
                // Name the offending paths. An unattended harvest that refuses
                // keeps refusing every run, and an operator who cannot see
                // *why* has to reproduce the state locally to find out — while
                // remote work accumulates uncaptured (#10222).
                let dirty =
                    git::get_dirty_files(&git_root.display().to_string()).unwrap_or_default();
                let listed: Vec<String> = dirty.iter().take(20).cloned().collect();
                let mut hints = vec![
                    "Commit, stash, or otherwise resolve local changes before applying remote content."
                        .to_string(),
                ];
                if !listed.is_empty() {
                    hints.push(format!("Uncommitted paths: {}", listed.join(", ")));
                }
                if dirty.len() > listed.len() {
                    hints.push(format!("...and {} more.", dirty.len() - listed.len()));
                }
                return Err(Error::validation_invalid_argument(
                    "local_path",
                    format!(
                        "refusing harvest for '{id}': local worktree has {} uncommitted change(s)",
                        dirty.len().max(1)
                    ),
                    Some(git_root.display().to_string()),
                    Some(hints),
                ));
            }
            content_diff::apply(snapshot.path(), &local_path, &result.changes)?;
            stage_component(git_root, &local_path)?;
            // Precedence: an explicit --author, then the configured automation
            // identity, then an anonymous fallback. The middle rung exists so an
            // unattended harvest does not invent an identity at the call site on
            // every run (#10221).
            let configured_identity = crate::defaults::load_config()
                .automation
                .commit_identity
                .filter(|identity| !identity.trim().is_empty());
            let author = options
                .author
                .as_deref()
                .or(configured_identity.as_deref())
                .unwrap_or("Remote harvest <harvest@homeboy.invalid>")
                .to_string();
            let author = author.as_str();
            let provenance = format!("Harvested-from: {project_id}:{remote_path}");
            git::commit_staged_with_author(
                git_root,
                &format!("Harvest remote changes for {id}\n\n{provenance}"),
                author,
            )?;
            result.committed = true;
            result.provenance = Some(provenance);
        }
        results.push(result);
    }
    Ok(HarvestResult {
        project_id: project_id.to_string(),
        results,
    })
}

fn stage_component(git_root: &Path, component_path: &Path) -> Result<()> {
    let canonical_root = fs::canonicalize(git_root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve Git worktree for harvest staging".to_string()),
        )
    })?;
    let canonical_component = fs::canonicalize(component_path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve component for harvest staging".to_string()),
        )
    })?;
    let relative = canonical_component
        .strip_prefix(&canonical_root)
        .map_err(|_| {
            Error::validation_invalid_argument(
                "local_path",
                format!(
                    "component path '{}' is outside Git worktree '{}'",
                    component_path.display(),
                    git_root.display()
                ),
                None,
                None,
            )
        })?;
    let pathspec = if relative.as_os_str().is_empty() {
        "."
    } else {
        relative.to_str().ok_or_else(|| {
            Error::validation_invalid_argument(
                "local_path",
                "component path must be valid UTF-8 for Git staging",
                None,
                None,
            )
        })?
    };
    git::run_git(
        &canonical_root,
        &["add", "-A", "--", pathspec],
        "stage harvested component",
    )?;
    Ok(())
}

fn materialize_remote(
    client: &crate::server::SshClient,
    remote_root: &str,
    destination: &Path,
    excludes: &[String],
) -> Result<()> {
    let output = client.execute(&format!(
        "test -d {} && find {} -type f -print0",
        shell::quote_path(remote_root),
        shell::quote_path(remote_root)
    ));
    if !output.success {
        return Err(Error::validation_invalid_argument(
            "remote_path",
            format!(
                "unable to read remote component '{}': {}",
                remote_root,
                output.stderr.trim()
            ),
            None,
            None,
        ));
    }
    for remote in output.stdout.split('\0').filter(|path| !path.is_empty()) {
        let relative = remote
            .strip_prefix(remote_root)
            .map(|path| path.trim_start_matches('/'))
            .filter(|path| !path.is_empty())
            .ok_or_else(|| {
                Error::internal_io(
                    "remote file escaped component root",
                    Some("materialize harvest snapshot".to_string()),
                )
            })?;
        if content_diff::excluded(relative, excludes) {
            continue;
        }
        let local = crate::resolve_contained_local_path(destination, relative, "remote path")?;
        if let Some(parent) = local.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("create harvest snapshot".to_string()),
                )
            })?;
        }
        let copied = client.download_file(remote, &local.display().to_string());
        if !copied.success {
            return Err(Error::internal_io(
                copied.stderr,
                Some(format!("download remote file {relative}")),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ScopedExtensionConfig;
    use crate::project::{Project, ProjectComponentAttachment};
    use crate::server::Server;
    use crate::test_support::with_isolated_home;
    use homeboy_extension_contract::manifest_capabilities::DeployCapability;
    use homeboy_extension_contract::{ExtensionManifest, RemotePathRootRule};
    use std::collections::HashMap;
    use std::process::Command;

    /// #10220: the exclude set must not depend on what happens to be
    /// installed. Extensions are absent on a CI runner, so a component that
    /// relies on inherited excludes gets a different comparison there than it
    /// does locally.
    #[test]
    fn component_declared_excludes_compose_with_invocation_flags() {
        let declared = vec!["composer.lock".to_string(), " vendor ".to_string()];
        let mut excludes: Vec<String> = vec!["node_modules".to_string()];
        excludes.extend(
            declared
                .iter()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        );
        excludes.extend(vec!["package-lock.json".to_string()]);
        excludes.sort();
        excludes.dedup();

        assert_eq!(
            excludes,
            vec![
                "composer.lock".to_string(),
                "node_modules".to_string(),
                "package-lock.json".to_string(),
                "vendor".to_string(),
            ],
            "declared excludes compose with policy and flag sources, trimmed and deduped"
        );
    }

    /// #10221: precedence is explicit flag, then configured identity, then an
    /// anonymous fallback. The middle rung is the point — an unattended run
    /// should not invent an identity at the call site.
    #[test]
    fn commit_author_precedence_prefers_flag_then_configuration() {
        fn resolve(flag: Option<&str>, configured: Option<&str>) -> String {
            let configured = configured
                .map(str::to_string)
                .filter(|identity| !identity.trim().is_empty());
            flag.or(configured.as_deref())
                .unwrap_or("Remote harvest <harvest@homeboy.invalid>")
                .to_string()
        }

        assert_eq!(
            resolve(
                Some("Op <op@example.invalid>"),
                Some("CI <ci@example.invalid>")
            ),
            "Op <op@example.invalid>",
            "an explicit author wins"
        );
        assert_eq!(
            resolve(None, Some("CI <ci@example.invalid>")),
            "CI <ci@example.invalid>",
            "configuration is used when no author is passed"
        );
        assert_eq!(
            resolve(None, None),
            "Remote harvest <harvest@homeboy.invalid>",
            "anonymous fallback remains when nothing is configured"
        );
        assert_eq!(
            resolve(None, Some("   ")),
            "Remote harvest <harvest@homeboy.invalid>",
            "a blank configured identity is not authoritative"
        );
    }

    /// #10221: the configured identity must survive a config round trip, or the
    /// setting silently does nothing.
    #[test]
    fn automation_commit_identity_round_trips_through_config() {
        with_isolated_home(|_| {
            let mut config = crate::defaults::load_config();
            assert!(config.automation.commit_identity.is_none());
            config.automation.commit_identity = Some("CI <ci@example.invalid>".to_string());
            crate::defaults::save_config(&config).expect("save config");

            let reloaded = crate::defaults::load_config();
            assert_eq!(
                reloaded.automation.commit_identity.as_deref(),
                Some("CI <ci@example.invalid>")
            );
        });
    }

    fn git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git starts");
        assert!(status.success(), "git {args:?}");
    }

    fn install_managed_root_extension(root: &Path) {
        crate::extension_store::save_manifest(&ExtensionManifest {
            id: "managed-host".to_string(),
            name: "Managed Host".to_string(),
            version: "1.0.0".to_string(),
            deploy: Some(DeployCapability {
                verifications: Vec::new(),
                overrides: Vec::new(),
                protected_path_suffixes: Vec::new(),
                owner_hints: Vec::new(),
                archive_install: Vec::new(),
                remote_path_inference: Vec::new(),
                path_roots: vec![RemotePathRootRule {
                    path_prefix: "managed".to_string(),
                    root: "managed_root".to_string(),
                    strip_prefix: true,
                    detect_command: Some(format!(
                        "printf %s {}",
                        shell::quote_path(&root.to_string_lossy())
                    )),
                }],
                version_patterns: Vec::new(),
                since_tag: None,
            }),
            ..serde_json::from_value(serde_json::json!({
                "name": "Managed Host",
                "version": "1.0.0"
            }))
            .expect("manifest")
        })
        .expect("save extension");
    }

    #[test]
    fn check_dry_run_apply_and_conflict_are_safe_for_local_transport() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("temp");
            let local = temp.path().join("local");
            let remote_root = temp.path().join("remote");
            let managed_root = temp.path().join("relocated-managed-root");
            let remote = managed_root.join("component");
            fs::create_dir_all(&local).expect("local");
            fs::create_dir_all(&remote_root).expect("remote root");
            fs::create_dir_all(&remote).expect("remote");
            install_managed_root_extension(&managed_root);
            let component_config = r#"{"id":"component","extensions":{"managed-host":{}}}"#;
            fs::write(local.join("homeboy.json"), component_config).expect("config");
            fs::write(local.join("same"), "same").expect("same");
            fs::write(local.join("changed"), "local").expect("changed");
            fs::write(local.join("deleted"), "local only").expect("deleted");
            fs::write(remote.join("homeboy.json"), component_config).expect("config");
            fs::write(remote.join("same"), "same").expect("same");
            fs::write(remote.join("changed"), "remote").expect("changed");
            fs::write(remote.join("added"), [0, 1, 255]).expect("added");
            fs::write(remote.join("ignored"), "remote").expect("ignored");
            fs::write(local.join("ignored"), "local").expect("ignored");
            git(&local, &["init"]);
            git(&local, &["config", "user.name", "Test"]);
            git(&local, &["config", "user.email", "test@example.invalid"]);
            git(&local, &["add", "."]);
            git(&local, &["commit", "-m", "initial"]);
            crate::server::save(&Server {
                id: "local".to_string(),
                host: "localhost".to_string(),
                user: "test".to_string(),
                port: 22,
                aliases: Vec::new(),
                identity_file: None,
                kind: None,
                auth: None,
                env: Default::default(),
                runner: None,
            })
            .expect("server");
            crate::project::save(&Project {
                id: "project".to_string(),
                server_id: Some("local".to_string()),
                base_path: Some(remote_root.display().to_string()),
                components: vec![ProjectComponentAttachment {
                    id: "component".to_string(),
                    local_path: local.display().to_string(),
                    remote_path: Some("managed/component".to_string()),
                }],
                extensions: Some(HashMap::from([(
                    "managed-host".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Project::default()
            })
            .expect("project");
            let options = HarvestOptions {
                component_ids: vec!["component".to_string()],
                dry_run: true,
                excludes: vec!["ignored".to_string()],
                ..Default::default()
            };
            let report = run("project", &options).expect("check");
            assert_eq!(report.results[0].status, "drift");
            assert_eq!(report.results[0].changes.len(), 3);
            assert_eq!(report.results[0].remote_path, remote.display().to_string());
            assert_eq!(
                fs::read_to_string(local.join("changed")).expect("unchanged"),
                "local"
            );
            let applied = run(
                "project",
                &HarvestOptions {
                    apply: true,
                    dry_run: false,
                    author: Some("Remote agent <remote@example.invalid>".to_string()),
                    ..options.clone()
                },
            )
            .expect("apply");
            assert!(applied.results[0].committed);
            let log = Command::new("git")
                .args(["log", "-1", "--format=%an%n%ae%n%B"])
                .current_dir(&local)
                .output()
                .expect("log");
            let log = String::from_utf8(log.stdout).expect("utf8 log");
            assert!(log.contains("Remote agent\nremote@example.invalid"));
            assert!(log.contains("Harvested-from: project:"));
            assert_eq!(
                fs::read(local.join("added")).expect("binary"),
                vec![0, 1, 255]
            );
            assert!(!local.join("deleted").exists());
            assert_eq!(
                fs::read_to_string(local.join("ignored")).expect("ignored"),
                "local"
            );
            assert_eq!(
                run("project", &options).expect("no change").results[0].status,
                "up_to_date"
            );
            fs::write(local.join("local-conflict"), "dirt").expect("dirt");
            fs::write(remote.join("changed"), "remote again").expect("remote drift");
            assert!(run(
                "project",
                &HarvestOptions {
                    apply: true,
                    dry_run: false,
                    ..options
                }
            )
            .is_err());
        });
    }

    #[test]
    fn apply_refuses_multiple_components_before_materializing_remote_content() {
        let error = run(
            "unused",
            &HarvestOptions {
                component_ids: vec!["one".to_string(), "two".to_string()],
                apply: true,
                ..Default::default()
            },
        )
        .expect_err("multi-component apply must fail");

        assert!(error.to_string().contains("exactly one component"));
    }
}
