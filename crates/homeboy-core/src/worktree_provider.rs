use crate::defaults::{self, HomeboyConfig};
use crate::error::{Error, Result};
use crate::{worktree, worktree_providers};
use std::path::{Path, PathBuf};

/// Canonical read-only ownership returned by every worktree provider.
///
/// Lifecycle mutation remains capability-segregated: native registry
/// reconciliation and command-provider finalization have different authority
/// models and are not implied by read-only ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeProviderIdentity {
    Native,
    Configured(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeOwnership {
    pub provider: WorktreeProviderIdentity,
    pub handle: String,
    pub path: String,
    pub branch: String,
    pub task_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeProviderLookup {
    Found(WorktreeOwnership),
    NotFound,
}

/// Provider-neutral read-only ownership contract.
///
/// `NotFound` means the provider authoritatively does not own the handle.
/// Corrupt state, malformed responses, timeouts, and unsafe workspaces are
/// errors and must never be treated as permission to fall through.
pub trait WorktreeProvider {
    fn resolve(&self, handle: &str) -> Result<WorktreeProviderLookup>;
}

/// Canonical local target admitted for a provider-owned mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeMutationTarget {
    pub provider: WorktreeProviderIdentity,
    pub handle: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeMutationLookup {
    Found(WorktreeMutationTarget),
    NotFound,
}

/// Mutable safety exceptions supplied by the lifecycle that owns the mutation.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorktreeMutationContext<'a> {
    pub safety_baseline: Option<&'a serde_json::Value>,
    pub trusted_unpushed_destination: Option<&'a worktree_providers::TrustedUnpushedWorktree>,
}

/// Optional lifecycle capability for resolving and revalidating a local
/// mutation target. Implementations retain authority over identity and safety.
pub trait WorktreeMutationProvider {
    fn resolve_for_mutation(
        &self,
        reference: &str,
        context: WorktreeMutationContext<'_>,
    ) -> Result<WorktreeMutationLookup>;
}

/// Built-in provider for Homeboy's standalone task-worktree registry.
///
/// This intentionally excludes adopted workspace refs because the first
/// migrated consumer historically admitted task worktrees only.
pub struct NativeWorktreeProvider;

impl WorktreeProvider for NativeWorktreeProvider {
    fn resolve(&self, handle: &str) -> Result<WorktreeProviderLookup> {
        let Some(record) = worktree::resolve_if_present(handle)? else {
            return Ok(WorktreeProviderLookup::NotFound);
        };
        if record.id != handle {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "native worktree registry record `{}` does not match requested handle `{handle}`",
                    record.id
                ),
                Some(handle.to_string()),
                None,
            ));
        }
        if record.state == worktree::TaskWorktreeState::Removed {
            return Ok(WorktreeProviderLookup::NotFound);
        }
        if record.branch.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!("native worktree `{handle}` has no branch"),
                Some(handle.to_string()),
                None,
            ));
        }
        let safety = worktree::safety_report_for_provider(&record)?;
        if safety.worktree_missing || !safety.safe {
            let mut reasons = safety.reasons;
            if safety.worktree_missing {
                reasons.push("worktree directory is missing".to_string());
            }
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!("native worktree `{handle}` is not safe for use"),
                Some(handle.to_string()),
                Some(reasons),
            ));
        }
        Ok(WorktreeProviderLookup::Found(WorktreeOwnership {
            provider: WorktreeProviderIdentity::Native,
            handle: record.id,
            path: record.worktree_path,
            branch: record.branch,
            task_url: record.task_url,
        }))
    }
}

impl WorktreeMutationProvider for NativeWorktreeProvider {
    fn resolve_for_mutation(
        &self,
        reference: &str,
        _context: WorktreeMutationContext<'_>,
    ) -> Result<WorktreeMutationLookup> {
        let Some(record) = worktree::resolve_workspace_ref_if_present(reference)? else {
            return Ok(WorktreeMutationLookup::NotFound);
        };
        if record.handle() != reference {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "native workspace registry record `{}` does not match requested handle `{reference}`",
                    record.handle()
                ),
                Some(reference.to_string()),
                None,
            ));
        }
        if record.state() != &worktree::TaskWorktreeState::Active {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "Homeboy workspace '{}' is no longer active",
                    record.handle()
                ),
                Some(reference.to_string()),
                None,
            ));
        }
        let path = PathBuf::from(record.path());
        if !path.is_dir() {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "Homeboy workspace '{}' points at a missing directory {}; recreate or remove the stale record",
                    record.handle(),
                    path.display()
                ),
                Some(reference.to_string()),
                None,
            ));
        }
        Ok(WorktreeMutationLookup::Found(WorktreeMutationTarget {
            provider: WorktreeProviderIdentity::Native,
            handle: record.handle().to_string(),
            path,
        }))
    }
}

/// Adapter for configured command-backed worktree providers.
pub struct CommandWorktreeProvider<'a> {
    config: &'a HomeboyConfig,
}

impl<'a> CommandWorktreeProvider<'a> {
    pub fn new(config: &'a HomeboyConfig) -> Self {
        Self { config }
    }
}

impl WorktreeProvider for CommandWorktreeProvider<'_> {
    fn resolve(&self, handle: &str) -> Result<WorktreeProviderLookup> {
        match worktree_providers::resolve_worktree_provider_from_config(handle, self.config) {
            Ok(resolution) => Ok(WorktreeProviderLookup::Found(WorktreeOwnership {
                provider: WorktreeProviderIdentity::Configured(resolution.provider_id),
                handle: resolution.worktree.handle,
                path: resolution.worktree.path,
                branch: resolution.worktree.branch,
                task_url: resolution.worktree.task_url,
            })),
            Err(error) if worktree_providers::is_worktree_provider_not_found(&error) => {
                Ok(WorktreeProviderLookup::NotFound)
            }
            Err(error) => Err(error),
        }
    }
}

impl WorktreeMutationProvider for CommandWorktreeProvider<'_> {
    fn resolve_for_mutation(
        &self,
        reference: &str,
        context: WorktreeMutationContext<'_>,
    ) -> Result<WorktreeMutationLookup> {
        let resolution = if Path::new(reference).is_dir() {
            worktree_providers::resolve_apply_enabled_worktree_provider_path_from_config(
                Path::new(reference),
                self.config,
                context.safety_baseline,
                context.trusted_unpushed_destination,
            )?
        } else {
            match worktree_providers::resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
                    reference,
                    self.config,
                    context.safety_baseline,
                    context.trusted_unpushed_destination,
                ) {
                Ok(resolution) => Some(resolution),
                Err(error) if worktree_providers::is_worktree_provider_not_found(&error) => None,
                Err(error) => return Err(error),
            }
        };
        Ok(match resolution {
            Some(resolution) => WorktreeMutationLookup::Found(WorktreeMutationTarget {
                provider: WorktreeProviderIdentity::Configured(resolution.provider_id),
                handle: resolution.worktree.handle,
                path: PathBuf::from(resolution.worktree.path),
            }),
            None => WorktreeMutationLookup::NotFound,
        })
    }
}

/// Resolve through the single ordered provider boundary used by consumers.
pub fn resolve_worktree_ownership(handle: &str) -> Result<WorktreeOwnership> {
    resolve_worktree_ownership_from_config(handle, &defaults::load_config())
}

pub fn resolve_worktree_ownership_from_config(
    handle: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeOwnership> {
    let native = NativeWorktreeProvider;
    if let WorktreeProviderLookup::Found(ownership) = native.resolve(handle)? {
        return Ok(ownership);
    }

    let command = CommandWorktreeProvider::new(config);
    if let WorktreeProviderLookup::Found(ownership) = command.resolve(handle)? {
        return Ok(ownership);
    }

    Err(worktree_providers::worktree_provider_not_found_error(
        handle, config, false,
    ))
}

/// Resolve a local mutation target through native ownership first, then the
/// configured command providers. Provider errors are authoritative and never
/// permit fallback.
pub fn resolve_worktree_mutation_target_from_config(
    reference: &str,
    config: &HomeboyConfig,
    context: WorktreeMutationContext<'_>,
) -> Result<WorktreeMutationTarget> {
    let native = NativeWorktreeProvider;
    if let WorktreeMutationLookup::Found(target) =
        native.resolve_for_mutation(reference, context)?
    {
        return Ok(target);
    }

    let command = CommandWorktreeProvider::new(config);
    if let WorktreeMutationLookup::Found(target) =
        command.resolve_for_mutation(reference, context)?
    {
        return Ok(target);
    }

    if Path::new(reference).is_dir() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "configured worktree providers do not own explicit destination path `{reference}`"
            ),
            Some(reference.to_string()),
            None,
        ));
    }
    Err(worktree_providers::worktree_provider_not_found_error(
        reference, config, true,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::*;
    use crate::defaults::{
        WorktreeProviderCommands, WorktreeProviderConfig, WorktreeProviderKind,
        WorktreeProviderListResultMapping,
    };

    fn assert_lookup_conformance(
        provider: &dyn WorktreeProvider,
        handle: &str,
        expected_provider: WorktreeProviderIdentity,
        expected_path: &Path,
    ) {
        let WorktreeProviderLookup::Found(ownership) =
            provider.resolve(handle).expect("owned handle resolves")
        else {
            panic!("owned handle was not found");
        };
        assert_eq!(ownership.provider, expected_provider);
        assert_eq!(ownership.handle, handle);
        assert_eq!(Path::new(&ownership.path), expected_path);
        assert!(matches!(
            provider
                .resolve("missing@worktree")
                .expect("missing lookup"),
            WorktreeProviderLookup::NotFound
        ));
    }

    fn assert_unsafe_lookup(provider: &dyn WorktreeProvider, handle: &str) {
        provider
            .resolve(handle)
            .expect_err("unsafe owned handle must fail instead of falling through");
    }

    fn assert_mutation_conformance(
        provider: &dyn WorktreeMutationProvider,
        handle: &str,
        expected_provider: WorktreeProviderIdentity,
        expected_path: &Path,
    ) {
        let resolve = || {
            let WorktreeMutationLookup::Found(target) = provider
                .resolve_for_mutation(handle, WorktreeMutationContext::default())
                .expect("owned mutation target resolves")
            else {
                panic!("owned mutation target was not found");
            };
            target
        };
        let admitted = resolve();
        let revalidated = resolve();
        assert_eq!(admitted, revalidated, "mutation identity must remain exact");
        assert_eq!(admitted.provider, expected_provider);
        assert_eq!(admitted.handle, handle);
        assert_eq!(admitted.path, expected_path);
        assert!(matches!(
            provider
                .resolve_for_mutation("missing@worktree", WorktreeMutationContext::default())
                .expect("missing mutation lookup"),
            WorktreeMutationLookup::NotFound
        ));
    }

    fn initialize_native_worktree(home: &Path) -> (tempfile::TempDir, std::path::PathBuf) {
        let source = tempfile::tempdir_in(home).expect("source checkout");
        let initialized = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(source.path())
            .output()
            .expect("initialize source repository");
        assert!(initialized.status.success());
        std::fs::write(source.path().join("README"), "fixture\n").expect("source file");
        let added = std::process::Command::new("git")
            .args(["add", "README"])
            .current_dir(source.path())
            .output()
            .expect("stage source file");
        assert!(added.status.success());
        let committed = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "fixture",
            ])
            .current_dir(source.path())
            .output()
            .expect("commit source file");
        assert!(committed.status.success());
        let path = home.join("native-worktree");
        let created = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "task/fixture-native",
                path.to_str().expect("UTF-8 worktree path"),
            ])
            .current_dir(source.path())
            .output()
            .expect("create native worktree");
        assert!(created.status.success());
        (source, path)
    }

    #[test]
    fn native_provider_conforms_to_shared_lookup_contract() {
        crate::test_support::with_isolated_home(|home| {
            let (source, path) = initialize_native_worktree(home.path());
            worktree::record_active_with_source_for_test("fixture@native", source.path(), &path);

            assert_lookup_conformance(
                &NativeWorktreeProvider,
                "fixture@native",
                WorktreeProviderIdentity::Native,
                &path,
            );
            assert_mutation_conformance(
                &NativeWorktreeProvider,
                "fixture@native",
                WorktreeProviderIdentity::Native,
                &path,
            );
            std::fs::write(path.join("dirty"), "dirty\n").expect("dirty native worktree");
            assert_unsafe_lookup(&NativeWorktreeProvider, "fixture@native");
        });
    }

    #[test]
    fn native_provider_treats_removed_records_as_not_found() {
        crate::test_support::with_isolated_home(|home| {
            let path = home.path().join("removed-worktree");
            worktree::record_removed_for_test("fixture@removed", &path);

            assert!(matches!(
                NativeWorktreeProvider
                    .resolve("fixture@removed")
                    .expect("removed lookup"),
                WorktreeProviderLookup::NotFound
            ));
        });
    }

    #[test]
    fn native_provider_rejects_colliding_manifest_identity() {
        crate::test_support::with_isolated_home(|home| {
            let path = home.path().join("colliding-worktree");
            worktree::record_removed_for_test("fixture@a/b", &path);

            let error = NativeWorktreeProvider
                .resolve("fixture@a?b")
                .expect_err("colliding handle must not resolve another manifest");
            assert!(error.message.contains("does not match requested handle"));
            let error = NativeWorktreeProvider
                .resolve_for_mutation("fixture@a?b", WorktreeMutationContext::default())
                .expect_err("colliding handle must not resolve another mutation target");
            assert!(error.message.contains("does not match requested handle"));
        });
    }

    #[test]
    fn command_provider_conforms_to_shared_lookup_contract() {
        crate::test_support::with_isolated_home(|_| {
            let workspace = tempfile::tempdir().expect("workspace");
            let initialized = std::process::Command::new("git")
                .args(["init", "-b", "command-branch"])
                .current_dir(workspace.path())
                .output()
                .expect("initialize git repository");
            assert!(initialized.status.success());

            let provider_dir = tempfile::tempdir().expect("provider directory");
            let script = provider_dir.path().join("provider");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' '{}'\n",
                    serde_json::json!({
                        "worktrees": [{
                            "handle": "fixture@command",
                            "path": workspace.path(),
                            "branch": "command-branch",
                            "safety": { "dirty": false, "unpushed": false, "primary": false }
                        }, {
                            "handle": "fixture@unsafe",
                            "path": workspace.path(),
                            "branch": "command-branch",
                            "safety": { "dirty": true, "unpushed": false, "primary": false }
                        }]
                    })
                ),
            )
            .expect("provider script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&script)
                    .expect("provider metadata")
                    .permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&script, permissions).expect("executable provider");
            }

            let mut providers = HashMap::new();
            providers.insert(
                "command-fixture".to_string(),
                WorktreeProviderConfig {
                    enabled: true,
                    kind: WorktreeProviderKind::Command,
                    apply_enabled: true,
                    commands: WorktreeProviderCommands {
                        list: Some(vec![script.display().to_string()]),
                        ..Default::default()
                    },
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    list_result_mapping: Some(WorktreeProviderListResultMapping {
                        items: "$.worktrees".to_string(),
                        handle: "$.handle".to_string(),
                        path: "$.path".to_string(),
                        branch: "$.branch".to_string(),
                        dirty: "$.safety.dirty".to_string(),
                        unpushed: "$.safety.unpushed".to_string(),
                        primary: "$.safety.primary".to_string(),
                        task_url: None,
                    }),
                },
            );
            let config = HomeboyConfig {
                worktree_providers: providers,
                ..HomeboyConfig::default()
            };

            assert_lookup_conformance(
                &CommandWorktreeProvider::new(&config),
                "fixture@command",
                WorktreeProviderIdentity::Configured("command-fixture".to_string()),
                workspace.path(),
            );
            assert_mutation_conformance(
                &CommandWorktreeProvider::new(&config),
                "fixture@command",
                WorktreeProviderIdentity::Configured("command-fixture".to_string()),
                workspace.path(),
            );
            assert_unsafe_lookup(&CommandWorktreeProvider::new(&config), "fixture@unsafe");
        });
    }
}
