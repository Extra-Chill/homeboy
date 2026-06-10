use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::core::rig;
use crate::core::{Error, Result};

use super::{
    exec, load, materialize_git_dependency, sync_workspace,
    workspace::{parent_remote_path, sanitize_path_segment},
    RunnerExecOptions, RunnerGitDependencyMaterializationOptions,
    RunnerGitDependencyMaterializationOutput, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RigComponentDependency {
    pub rig_id: String,
    pub component_id: String,
    pub local_checkout_root: String,
    pub declared_checkout_root: String,
    pub remote_checkout_root: String,
    pub required_subpath: Option<String>,
    pub remote_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct LabOffloadRigSync {
    pub rig_id: String,
    pub source: String,
    pub source_kind: LabOffloadRigSyncSource,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LabOffloadRigSyncSource {
    PrimarySnapshot,
    InstalledMetadata,
}

pub(super) fn sync_lab_offload_rigs(
    runner_id: &str,
    command_path: &str,
    remote_cwd: &str,
    args: &[String],
    primary_local_path: &str,
    primary_remote_path: &str,
) -> Result<Vec<LabOffloadRigSync>> {
    let rig_ids = lab_offload_rig_ids(args);
    if rig_ids.is_empty() {
        return Ok(Vec::new());
    }

    let primary_rig_ids = primary_source_rig_ids(primary_local_path)?;
    let mut synced_rigs = Vec::new();
    for rig_id in &rig_ids {
        let (source, source_kind) = if primary_rig_ids.contains(rig_id) {
            (
                primary_remote_path.to_string(),
                LabOffloadRigSyncSource::PrimarySnapshot,
            )
        } else {
            let metadata = rig::read_source_metadata(rig_id).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "rig",
                    format!(
                        "runner dispatch cannot materialize rig `{rig_id}` because it has no installed source metadata"
                    ),
                    Some(rig_id.clone()),
                    Some(vec![
                        format!("Reinstall rig `{rig_id}` from a rig package before using --runner."),
                        "Run the rig sources command to inspect installed rig sources.".to_string(),
                    ]),
                )
            })?;
            let synced = sync_workspace(
                runner_id,
                RunnerWorkspaceSyncOptions {
                    path: metadata.package_path,
                    mode: RunnerWorkspaceSyncMode::Snapshot,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                },
            )?
            .0;
            (
                synced.remote_path,
                LabOffloadRigSyncSource::InstalledMetadata,
            )
        };

        let (output, exit_code) = exec(
            runner_id,
            RunnerExecOptions {
                cwd: Some(remote_cwd.to_string()),
                project_id: None,
                allow_diagnostic_ssh: false,
                command: vec![
                    command_path.to_string(),
                    "rig".to_string(),
                    "install".to_string(),
                    source.clone(),
                    "--id".to_string(),
                    rig_id.clone(),
                ],
                env: HashMap::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: Vec::new(),
            },
        )?;

        if exit_code != 0 {
            return Err(Error::validation_invalid_argument(
                "rig",
                format!("runner dispatch could not install rig `{rig_id}` on runner `{runner_id}`"),
                Some(rig_id.clone()),
                Some(vec![
                    output.stderr.trim().to_string(),
                    "Run the command with --force-hot to execute locally while investigating runner rig setup.".to_string(),
                    format!("Lab source snapshot remote path: {primary_remote_path}"),
                    format!("Selected rig install source: {source}"),
                ]),
            ));
        }

        synced_rigs.push(LabOffloadRigSync {
            rig_id: rig_id.clone(),
            source,
            source_kind,
        });
    }

    Ok(synced_rigs)
}

pub(super) fn remap_bench_rig_default_component_to_primary_snapshot(
    args: &[String],
    primary_remote_path: &str,
) -> Vec<String> {
    if !is_bench_rig_run(args) || has_path_arg(args) {
        return args.to_vec();
    }

    let mut out = Vec::with_capacity(args.len() + 2);
    let mut inserted = false;
    let mut passthrough = false;
    for arg in args {
        if !inserted && !passthrough && arg == "--" {
            out.push("--path".to_string());
            out.push(primary_remote_path.to_string());
            inserted = true;
        }
        if arg == "--" {
            passthrough = true;
        }
        out.push(arg.clone());
    }
    if !inserted {
        out.push("--path".to_string());
        out.push(primary_remote_path.to_string());
    }
    out
}

fn primary_source_rig_ids(primary_local_path: &str) -> Result<HashSet<String>> {
    let path = Path::new(primary_local_path);
    if !path.join("rig.json").is_file() && !path.join("rigs").is_dir() {
        return Ok(HashSet::new());
    }
    Ok(rig::discover_rigs(path)?
        .into_iter()
        .map(|discovered| discovered.id)
        .collect())
}

fn is_bench_rig_run(args: &[String]) -> bool {
    matches!(args.get(1).map(String::as_str), Some("bench"))
        && !lab_offload_rig_ids(args).is_empty()
}

fn has_path_arg(args: &[String]) -> bool {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            return false;
        }
        if arg == "--path" {
            return true;
        }
        if arg.starts_with("--path=") {
            return true;
        }
    }
    false
}

pub(super) fn sync_lab_offload_rig_component_dependencies(
    runner_id: &str,
    args: &[String],
    primary_local_path: &str,
    primary_remote_path: &str,
) -> Result<Vec<RunnerGitDependencyMaterializationOutput>> {
    let dependencies = lab_offload_rig_component_dependencies(
        args,
        Some((primary_local_path, primary_remote_path)),
    )?;
    if dependencies.is_empty() {
        return Ok(Vec::new());
    }

    let runner = load(runner_id)?;
    let mut synced = Vec::new();
    let mut seen = HashSet::new();
    for dependency in dependencies {
        if !should_materialize_dependency(&dependency, primary_remote_path) {
            continue;
        }
        if !seen.insert(dependency.remote_checkout_root.clone()) {
            continue;
        }
        synced.push(materialize_git_dependency(
            &runner,
            RunnerGitDependencyMaterializationOptions {
                local_path: dependency.local_checkout_root,
                remote_path: dependency.remote_checkout_root,
                remote_url: dependency.remote_url,
                required_subpath: dependency.required_subpath,
            },
        )?);
    }

    Ok(synced)
}

pub(super) fn lab_offload_rig_component_dependencies(
    args: &[String],
    primary_workspace: Option<(&str, &str)>,
) -> Result<Vec<RigComponentDependency>> {
    let mut dependencies = Vec::new();
    for rig_id in lab_offload_rig_ids(args) {
        let spec = rig::load(&rig_id)?;
        for (component_id, component) in &spec.components {
            let checkout_root = component
                .checkout_root
                .as_deref()
                .unwrap_or(component.path.as_str());
            let local_checkout_root = expanded_local_path(&spec, checkout_root);
            let local_component_path = expanded_local_path(&spec, &component.path);
            let required_subpath = required_component_subpath(
                Path::new(&local_checkout_root),
                Path::new(&local_component_path),
                &rig_id,
                component_id,
            )?;
            dependencies.push(RigComponentDependency {
                rig_id: rig_id.clone(),
                component_id: component_id.clone(),
                remote_checkout_root: remote_checkout_root_for_local(
                    checkout_root,
                    &local_checkout_root,
                    primary_workspace,
                ),
                local_checkout_root,
                declared_checkout_root: checkout_root.to_string(),
                required_subpath,
                remote_url: component.remote_url.clone(),
            });
        }
    }
    Ok(dependencies)
}

fn expanded_local_path(spec: &rig::RigSpec, value: &str) -> String {
    rig::expand::expand_vars(spec, value)
}

fn remote_checkout_root_for_local(
    declared_checkout_root: &str,
    local_checkout_root: &str,
    primary_workspace: Option<(&str, &str)>,
) -> String {
    let Some((primary_local_path, primary_remote_path)) = primary_workspace else {
        return local_checkout_root.to_string();
    };
    if normalize_path_for_prefix(Path::new(local_checkout_root))
        == normalize_path_for_prefix(Path::new(primary_local_path))
    {
        return primary_remote_path.to_string();
    }
    if is_portable_runner_path(declared_checkout_root) {
        return declared_checkout_root.to_string();
    }
    let name = Path::new(local_checkout_root)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("dependency");
    format!(
        "{}/{}",
        parent_remote_path(primary_remote_path),
        sanitize_path_segment(name)
    )
}

fn is_portable_runner_path(path: &str) -> bool {
    path == "~" || path.starts_with("~/")
}

fn should_materialize_dependency(
    dependency: &RigComponentDependency,
    primary_remote_path: &str,
) -> bool {
    dependency.remote_checkout_root != primary_remote_path
}

fn required_component_subpath(
    checkout_root: &Path,
    component_path: &Path,
    rig_id: &str,
    component_id: &str,
) -> Result<Option<String>> {
    let checkout_root = normalize_path_for_prefix(checkout_root);
    let component_path = normalize_path_for_prefix(component_path);
    if checkout_root == component_path {
        return Ok(None);
    }
    let subpath = component_path.strip_prefix(&checkout_root).map_err(|_| {
        Error::validation_invalid_argument(
            "checkout_root",
            format!(
                "rig `{rig_id}` component `{component_id}` declares checkout_root outside its component path"
            ),
            Some(checkout_root.display().to_string()),
            Some(vec![format!(
                "Set checkout_root to the repository root that contains {}.",
                component_path.display()
            )]),
        )
    })?;
    Ok(Some(subpath.display().to_string()))
}

fn normalize_path_for_prefix(path: &Path) -> PathBuf {
    path.components().collect()
}

fn lab_offload_rig_ids(args: &[String]) -> Vec<String> {
    let mut rig_ids = Vec::new();

    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            continue;
        }
        if arg == "--" {
            passthrough = true;
            continue;
        }
        let raw = if arg == "--rig" {
            iter.next().map(String::as_str)
        } else {
            arg.strip_prefix("--rig=")
        };
        if let Some(raw) = raw {
            for rig_id in raw
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                push_unique(&mut rig_ids, rig_id.to_string());
            }
        }
    }

    rig_ids
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_unique_bench_rig_ids_for_lab_materialization() {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig".to_string(),
            "baseline,candidate".to_string(),
            "--scenario".to_string(),
            "smoke".to_string(),
            "--rig=candidate".to_string(),
        ];

        assert_eq!(
            lab_offload_rig_ids(&args),
            vec!["baseline".to_string(), "candidate".to_string()]
        );
    }

    #[test]
    fn extracts_trace_rig_ids_and_ignores_passthrough_args_for_lab_materialization() {
        let trace = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--rig".to_string(),
            "candidate".to_string(),
        ];
        assert_eq!(lab_offload_rig_ids(&trace), vec!["candidate".to_string()]);

        let passthrough = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--".to_string(),
            "--rig".to_string(),
            "candidate".to_string(),
        ];
        assert!(lab_offload_rig_ids(&passthrough).is_empty());
    }

    #[test]
    fn collects_rig_component_dependency_checkout_roots() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("Developer/woocommerce");
            std::fs::create_dir_all(checkout.join("plugins/woocommerce")).expect("checkout");
            let rig_dir = crate::core::paths::rigs().expect("rig dir");
            std::fs::create_dir_all(&rig_dir).expect("create rig dir");
            std::fs::write(
                rig_dir.join("woocommerce-performance.json"),
                serde_json::json!({
                    "id": "woocommerce-performance",
                    "components": {
                        "woocommerce": {
                            "path": format!("{}/plugins/woocommerce", checkout.display()),
                            "checkout_root": checkout.display().to_string(),
                            "remote_url": "https://github.com/woocommerce/woocommerce.git"
                        }
                    }
                })
                .to_string(),
            )
            .expect("save rig");

            let dependencies = lab_offload_rig_component_dependencies(
                &[
                    "homeboy".to_string(),
                    "bench".to_string(),
                    "--rig".to_string(),
                    "woocommerce-performance".to_string(),
                ],
                None,
            )
            .expect("dependencies");

            assert_eq!(dependencies.len(), 1);
            assert_eq!(dependencies[0].component_id, "woocommerce");
            assert_eq!(
                dependencies[0].local_checkout_root,
                checkout.display().to_string()
            );
            assert_eq!(
                dependencies[0].declared_checkout_root,
                checkout.display().to_string()
            );
            assert_eq!(
                dependencies[0].remote_checkout_root,
                checkout.display().to_string()
            );
            assert_eq!(
                dependencies[0].required_subpath.as_deref(),
                Some("plugins/woocommerce")
            );
        });
    }

    #[test]
    fn expands_package_root_for_remote_component_dependency_root() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("Developer/studio-web");
            std::fs::create_dir_all(checkout.join("rigs/studio-web-product-matrix"))
                .expect("rig package");
            let rig_dir = crate::core::paths::rigs().expect("rig dir");
            std::fs::create_dir_all(&rig_dir).expect("create rig dir");
            std::fs::write(
                rig_dir.join("studio-web-product-matrix.json"),
                serde_json::json!({
                    "id": "studio-web-product-matrix",
                    "components": {
                        "studio-web": {
                            "path": "${package.root}",
                            "remote_url": "https://github.a8c.com/chubes4/studio-web.git"
                        }
                    }
                })
                .to_string(),
            )
            .expect("save rig");
            std::fs::create_dir_all(crate::core::paths::rig_sources().expect("rig sources"))
                .expect("create rig sources");
            crate::core::rig::install::write_source_metadata(
                "studio-web-product-matrix",
                &crate::core::rig::install::RigSourceMetadata {
                    source: checkout.display().to_string(),
                    package_path: checkout.display().to_string(),
                    rig_path: checkout
                        .join("rigs/studio-web-product-matrix/rig.json")
                        .display()
                        .to_string(),
                    discovery_path: Some(checkout.display().to_string()),
                    source_revision: None,
                    linked: true,
                },
            )
            .expect("source metadata");

            let dependencies = lab_offload_rig_component_dependencies(
                &[
                    "homeboy".to_string(),
                    "bench".to_string(),
                    "--rig".to_string(),
                    "studio-web-product-matrix".to_string(),
                ],
                Some((
                    &checkout.display().to_string(),
                    "/home/chubes/Developer/_lab_workspaces/studio-web-snapshot",
                )),
            )
            .expect("dependencies");

            assert_eq!(dependencies.len(), 1);
            assert_eq!(
                dependencies[0].local_checkout_root,
                checkout.display().to_string()
            );
            assert_eq!(
                dependencies[0].remote_checkout_root,
                "/home/chubes/Developer/_lab_workspaces/studio-web-snapshot"
            );
            assert!(!dependencies[0]
                .remote_checkout_root
                .contains("${package.root}"));
        });
    }

    #[test]
    fn primary_source_rig_ids_discovers_rigs_from_current_source_tree() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("Developer/studio-web-release-clean");
            let rig_dir = checkout.join("rigs/studio-web-product-matrix");
            std::fs::create_dir_all(&rig_dir).expect("rig dir");
            std::fs::write(
                rig_dir.join("rig.json"),
                serde_json::json!({
                    "id": "studio-web-product-matrix",
                    "components": {},
                    "bench": { "default_component": "studio-web" }
                })
                .to_string(),
            )
            .expect("rig spec");

            let rig_ids =
                primary_source_rig_ids(&checkout.display().to_string()).expect("primary rigs");

            assert!(rig_ids.contains("studio-web-product-matrix"));
        });
    }

    #[test]
    fn bench_rig_default_component_args_receive_primary_snapshot_path() {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig".to_string(),
            "studio-web-product-matrix".to_string(),
            "--scenario".to_string(),
            "editable_preview_ready".to_string(),
        ];

        let rewritten = remap_bench_rig_default_component_to_primary_snapshot(
            &args,
            "/home/chubes/Developer/_lab_workspaces/studio-web-release-clean-abc",
        );

        assert_eq!(
            rewritten,
            vec![
                "homeboy",
                "bench",
                "--rig",
                "studio-web-product-matrix",
                "--scenario",
                "editable_preview_ready",
                "--path",
                "/home/chubes/Developer/_lab_workspaces/studio-web-release-clean-abc",
            ]
        );
    }

    #[test]
    fn bench_rig_path_injection_preserves_passthrough_boundary() {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig=studio-web-product-matrix".to_string(),
            "--".to_string(),
            "--runner-owned".to_string(),
        ];

        let rewritten = remap_bench_rig_default_component_to_primary_snapshot(
            &args,
            "/home/chubes/Developer/_lab_workspaces/studio-web-release-clean-abc",
        );

        assert_eq!(
            rewritten,
            vec![
                "homeboy",
                "bench",
                "--rig=studio-web-product-matrix",
                "--path",
                "/home/chubes/Developer/_lab_workspaces/studio-web-release-clean-abc",
                "--",
                "--runner-owned",
            ]
        );
    }

    #[test]
    fn bench_rig_path_injection_keeps_explicit_path() {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig".to_string(),
            "studio-web-product-matrix".to_string(),
            "--path".to_string(),
            "/custom/source".to_string(),
        ];

        assert_eq!(
            remap_bench_rig_default_component_to_primary_snapshot(&args, "/snapshot"),
            args
        );
    }

    #[test]
    fn maps_non_primary_rig_dependency_to_runner_workspace_parent() {
        let remote = remote_checkout_root_for_local(
            "/Users/chubes/Developer/studio@fix-many-sites-memory",
            "/Users/chubes/Developer/studio@fix-many-sites-memory",
            Some((
                "/Users/chubes/Developer/homeboy-rigs/Automattic/studio",
                "/home/chubes/Developer/_lab_workspaces/studio-rigs-snapshot",
            )),
        );

        assert_eq!(
            remote,
            "/home/chubes/Developer/_lab_workspaces/studio-fix-many-sites-memory"
        );
        assert!(!remote.contains("/Users/"));
    }

    #[test]
    fn preserves_portable_declared_rig_dependency_path_for_runner() {
        let remote = remote_checkout_root_for_local(
            "~/Developer/studio@fix-many-sites-memory",
            "/Users/chubes/Developer/studio@fix-many-sites-memory",
            Some((
                "/Users/chubes/Developer/homeboy-rigs/Automattic/studio",
                "/home/chubes/Developer/_lab_workspaces/studio-rigs-snapshot",
            )),
        );

        assert_eq!(remote, "~/Developer/studio@fix-many-sites-memory");
    }

    #[test]
    fn primary_workspace_dependency_is_not_materialized_again() {
        let primary_remote_path = "/home/chubes/Developer/_lab_workspaces/studio-web-snapshot";
        let dependencies = vec![RigComponentDependency {
            rig_id: "studio-web-product-matrix".to_string(),
            component_id: "studio-web".to_string(),
            local_checkout_root: "/Users/chubes/Developer/studio-web".to_string(),
            declared_checkout_root: "/Users/chubes/Developer/studio-web".to_string(),
            remote_checkout_root: primary_remote_path.to_string(),
            required_subpath: None,
            remote_url: Some("https://github.a8c.com/chubes4/studio-web.git".to_string()),
        }];

        assert!(dependencies
            .into_iter()
            .filter(|dependency| should_materialize_dependency(dependency, primary_remote_path))
            .collect::<Vec<_>>()
            .is_empty());
    }

    #[test]
    fn rejects_checkout_root_outside_component_path() {
        let err = required_component_subpath(
            Path::new("/tmp/wordpress"),
            Path::new("/tmp/woocommerce/plugins/woocommerce"),
            "rig",
            "woocommerce",
        )
        .expect_err("root outside component path");

        assert_eq!(err.details["field"], "checkout_root");
        assert!(err.message.contains("component `woocommerce`"));
    }
}
