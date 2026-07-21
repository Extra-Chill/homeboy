//! Isolated, content-addressed agent-runtime generation materialization.
//!
//! This module deliberately does not select or offload a provider. Its caller
//! supplies an already validated v2 plan and consumes the resolved generation.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use homeboy_agents::agent_task_dispatch_service::ResolvedAgentTaskProviderPolicy;
use homeboy_agents::agent_tasks::provider::AgentTaskExecutorProvider;
use homeboy_core::agent_runtime_manifest::{
    validate_runtime_materialization_plan, AgentRuntimeMaterializationPlan,
    AgentRuntimeSourceLocator,
};
use homeboy_core::error::{Error, Result};

use crate::{copy_snapshot_to_directory, exec, Runner, RunnerExecOptions, RunnerKind};

const RECORD_FILE: &str = ".homeboy-agent-runtime-generation.json";
const STAGING_SUFFIX: &str = ".staging";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedAgentRuntimeGeneration {
    pub generation_identity: String,
    pub content_identity: String,
    pub immutable_root: String,
    pub runtime_path: String,
    pub provider_id: String,
    pub requested_runtime_id: String,
    pub requested_source_revision: Option<String>,
    pub resolved_source_identities: Vec<ResolvedAgentRuntimeSourceIdentity>,
    pub publication: AgentRuntimeGenerationPublication,
    pub cleanup: AgentRuntimeGenerationCleanup,
}

/// Serializable, versioned proof of the runtime identity at each handoff.
/// `executed` is derived from the rewritten dispatch argument, never from the
/// original controller declaration, so it proves direct and reverse paths use
/// the same immutable generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeExecutionEvidence {
    pub schema: String,
    pub requested: AgentRuntimeEvidenceIdentity,
    pub resolved: AgentRuntimeEvidenceIdentity,
    pub executed: AgentRuntimeEvidenceIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeEvidenceIdentity {
    pub runtime_id: String,
    pub provider_id: String,
    pub source_revision: Option<String>,
    pub content_identity: String,
    pub build_identity: String,
    pub runtime_path: String,
    pub generation: String,
    pub admission: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedAgentRuntimeSourceIdentity {
    pub id: String,
    pub requested_content_identity: String,
    pub resolved_content_identity: String,
    pub destination_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeGenerationPublication {
    Reused,
    Published,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeGenerationCleanup {
    Immutable,
    StagingRemoved,
}

/// The only runtime mutation allowed at Lab handoff: a copy of the
/// controller-selected provider, pointed at the immutable generation for this
/// job. The runner's discovered provider and every active generation remain
/// untouched.
#[derive(Debug, Clone)]
pub(crate) struct LabResolvedAgentRuntime {
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) generation: ResolvedAgentRuntimeGeneration,
}

/// Resolve a v2 controller runtime declaration after its source workspaces have
/// reached the runner. Older declarations deliberately return `None`: their
/// caller retains the exact advertised-runtime preflight contract.
pub(crate) fn resolve_lab_agent_runtime(
    operations: &mut impl AgentRuntimeMaterializerOperations,
    runner: &Runner,
    args: &[String],
    workspace_remaps: &[(String, String)],
    job_or_run_id: &str,
) -> Result<Option<LabResolvedAgentRuntime>> {
    let Some((policy_index, raw_policy)) = resolved_provider_policy_arg(args) else {
        return Ok(None);
    };
    let mut policy: ResolvedAgentTaskProviderPolicy =
        serde_json::from_str(raw_policy).map_err(|error| {
            Error::validation_invalid_argument(
                "resolved-provider-policy",
                "Lab received an invalid controller provider policy",
                Some(error.to_string()),
                None,
            )
        })?;
    let Some(identity) = policy.runtime_identity.as_mut() else {
        return Ok(None);
    };
    let mut plan: AgentRuntimeMaterializationPlan =
        match serde_json::from_value::<AgentRuntimeMaterializationPlan>(
            identity.materialization_plan.clone(),
        ) {
            Ok(plan)
                if plan.schema == "homeboy/agent-runtime-materialization-plan/v2"
                    && !plan.runtime_sources.is_empty() =>
            {
                plan
            }
            _ => return Ok(None),
        };
    // Runtime sources are controller-owned immutable inputs, not workspace
    // aliases. In particular, a reverse runner must not receive a controller
    // path merely because the normal command workspace was synchronized.
    let _ = workspace_remaps;
    let generation =
        materialize_agent_runtime_generation(operations, &plan, runner, job_or_run_id)?;
    let mut provider: AgentTaskExecutorProvider = serde_json::from_value(identity.provider.clone())
        .map_err(|error| {
            Error::validation_invalid_argument(
                "resolved-provider-policy",
                "Lab received an invalid controller-selected provider",
                Some(error.to_string()),
                None,
            )
        })?;
    if provider.id != identity.provider_id || provider.id != generation.provider_id {
        return Err(Error::validation_invalid_argument(
            "resolved-provider-policy",
            "materialized runtime provider does not match the controller-selected provider",
            Some(generation.provider_id.clone()),
            None,
        ));
    }
    provider.runtime_path = Some(generation.runtime_path.clone());
    identity.provider = serde_json::to_value(provider).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize resolved Lab provider".to_string()),
        )
    })?;
    plan.runtime_path = Some(generation.runtime_path.clone());
    // The policy crosses the runner boundary after this point. Replace every
    // controller locator with its published runner generation path so reverse
    // submission can neither observe nor accidentally reuse controller paths.
    for source in &mut plan.runtime_sources {
        source.locator = AgentRuntimeSourceLocator::LocalPath {
            path: Path::new(&generation.immutable_root)
                .join(&source.destination_path)
                .display()
                .to_string(),
        };
    }
    identity.materialization_plan = serde_json::to_value(plan).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize resolved Lab runtime plan".to_string()),
        )
    })?;
    let raw_policy = serde_json::to_string(&policy).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize resolved Lab provider policy".to_string()),
        )
    })?;
    let mut args = args.to_vec();
    args[policy_index] = raw_policy;
    Ok(Some(LabResolvedAgentRuntime {
        args,
        env: vec![
            (
                "HOMEBOY_RUNTIME_PATH".to_string(),
                generation.runtime_path.clone(),
            ),
            (
                "HOMEBOY_AGENT_RUNTIME_PATH".to_string(),
                generation.runtime_path.clone(),
            ),
        ],
        generation,
    }))
}

fn resolved_provider_policy_arg(args: &[String]) -> Option<(usize, &str)> {
    args.iter().enumerate().find_map(|(index, arg)| {
        arg.strip_prefix("--resolved-provider-policy=")
            .map(|value| (index, value))
            .or_else(|| {
                (arg == "--resolved-provider-policy")
                    .then(|| args.get(index + 1).map(|value| (index + 1, value.as_str())))
                    .flatten()
            })
    })
}

/// Construct evidence only after the final dispatch argv has been rewritten.
/// This is the pre-submit equality gate: a mismatched policy fails before a
/// daemon admission can occur.
pub(crate) fn runtime_execution_evidence(
    generation: &ResolvedAgentRuntimeGeneration,
    executed_args: &[String],
    admission: &str,
) -> Result<AgentRuntimeExecutionEvidence> {
    let (_, raw_policy) = resolved_provider_policy_arg(executed_args).ok_or_else(|| {
        invalid(
            "executed_runtime",
            "materialized runtime dispatch has no resolved provider policy",
        )
    })?;
    let policy: ResolvedAgentTaskProviderPolicy =
        serde_json::from_str(raw_policy).map_err(|_| {
            invalid(
                "executed_runtime",
                "materialized runtime dispatch has an invalid resolved provider policy",
            )
        })?;
    let identity = policy.runtime_identity.ok_or_else(|| {
        invalid(
            "executed_runtime",
            "materialized runtime dispatch has no runtime identity",
        )
    })?;
    let plan: AgentRuntimeMaterializationPlan =
        serde_json::from_value(identity.materialization_plan)
            .map_err(|_| invalid("executed_runtime", "executed runtime plan is invalid"))?;
    let executed = AgentRuntimeEvidenceIdentity {
        runtime_id: plan.runtime_id.clone(),
        provider_id: identity.provider_id,
        source_revision: plan.source_revision.clone(),
        content_identity: generation.content_identity.clone(),
        build_identity: generation.generation_identity.clone(),
        runtime_path: plan.runtime_path.unwrap_or_default(),
        generation: generation.generation_identity.clone(),
        admission: admission.to_string(),
    };
    let resolved = AgentRuntimeEvidenceIdentity {
        runtime_id: generation.requested_runtime_id.clone(),
        provider_id: generation.provider_id.clone(),
        source_revision: generation.requested_source_revision.clone(),
        content_identity: generation.content_identity.clone(),
        build_identity: generation.generation_identity.clone(),
        runtime_path: generation.runtime_path.clone(),
        generation: generation.generation_identity.clone(),
        admission: admission.to_string(),
    };
    if executed != resolved {
        return Err(invalid(
            "executed_runtime",
            "executed runtime identity does not equal the resolved immutable generation",
        ));
    }
    Ok(AgentRuntimeExecutionEvidence {
        schema: "homeboy/agent-runtime-execution-evidence/v1".to_string(),
        requested: AgentRuntimeEvidenceIdentity {
            runtime_id: generation.requested_runtime_id.clone(),
            provider_id: generation.provider_id.clone(),
            source_revision: generation.requested_source_revision.clone(),
            content_identity: generation.content_identity.clone(),
            build_identity: generation.generation_identity.clone(),
            runtime_path: String::new(),
            generation: generation.generation_identity.clone(),
            admission: "requested".to_string(),
        },
        resolved,
        executed,
    })
}

/// Small boundary around runner storage and process execution. It makes the
/// materialization transaction deterministic to test without selecting a Lab
/// transport or mutating a controller source checkout.
pub trait AgentRuntimeMaterializerOperations {
    fn generation_root(&mut self, runner: &Runner, generation_identity: &str) -> Result<PathBuf>;
    fn source_exists(&mut self, source: &Path) -> Result<bool>;
    fn source_revision(&mut self, source: &Path) -> Result<String>;
    fn snapshot_source(&mut self, source: &Path, destination: &Path) -> Result<()>;
    fn create_dir_all(&mut self, path: &Path) -> Result<()>;
    fn path_exists(&mut self, path: &Path) -> Result<bool>;
    fn read_record(&mut self, root: &Path) -> Result<Option<ResolvedAgentRuntimeGeneration>>;
    fn write_record(
        &mut self,
        root: &Path,
        generation: &ResolvedAgentRuntimeGeneration,
    ) -> Result<()>;
    fn run_argv(&mut self, runner: &Runner, argv: &[String], cwd: &Path) -> Result<()>;
    fn rename_publish(&mut self, staging: &Path, destination: &Path) -> Result<()>;
    fn remove_tree(&mut self, path: &Path) -> Result<()>;
}

/// Injectable boundary for the runner-facing half of runtime materialization.
/// Local snapshot construction remains local, while every remote command and
/// transfer is recorded or executed through this transport.
pub trait RunnerRuntimeMaterializerTransport {
    fn exec(&self, runner: &Runner, argv: Vec<String>, cwd: String) -> Result<i32>;
    fn ensure_directory(&self, runner: &Runner, path: &str) -> Result<()>;
    fn upload_file(&self, runner: &Runner, local_path: &str, remote_path: &str) -> Result<()>;
    fn download_file(&self, runner: &Runner, remote_path: &str, local_path: &str) -> Result<()>;
}

pub struct NativeRunnerRuntimeMaterializerTransport;

impl RunnerRuntimeMaterializerTransport for NativeRunnerRuntimeMaterializerTransport {
    fn exec(&self, runner: &Runner, argv: Vec<String>, cwd: String) -> Result<i32> {
        exec(
            &runner.id,
            RunnerExecOptions::raw_command(argv).with_cwd(cwd),
        )
        .map(|(_, exit_code)| exit_code)
    }

    fn ensure_directory(&self, runner: &Runner, path: &str) -> Result<()> {
        crate::RunnerFileTransfer::for_runner(runner, crate::status(&runner.id).ok().as_ref())?
            .ensure_directory(path)
    }

    fn upload_file(&self, runner: &Runner, local_path: &str, remote_path: &str) -> Result<()> {
        crate::RunnerFileTransfer::for_runner(runner, crate::status(&runner.id).ok().as_ref())?
            .upload_file(local_path, remote_path)
    }

    fn download_file(&self, runner: &Runner, remote_path: &str, local_path: &str) -> Result<()> {
        crate::RunnerFileTransfer::for_runner(runner, crate::status(&runner.id).ok().as_ref())?
            .download_file(remote_path, local_path)
    }
}

/// Production operations backed by the runner workspace snapshot and exec
/// primitives.
pub struct RunnerRuntimeMaterializerOperations<T = NativeRunnerRuntimeMaterializerTransport> {
    runner: Runner,
    transport: T,
}

impl RunnerRuntimeMaterializerOperations<NativeRunnerRuntimeMaterializerTransport> {
    pub(crate) fn new(runner: Runner) -> Self {
        Self {
            runner,
            transport: NativeRunnerRuntimeMaterializerTransport,
        }
    }
}

impl<T: RunnerRuntimeMaterializerTransport> RunnerRuntimeMaterializerOperations<T> {
    #[cfg(test)]
    fn with_transport(runner: Runner, transport: T) -> Self {
        Self { runner, transport }
    }

    fn remote_exec(&self, argv: Vec<String>, cwd: Option<&Path>) -> Result<()> {
        let exit_code = self.transport.exec(
            &self.runner,
            argv,
            cwd.map(|path| path.display().to_string())
                .or_else(|| self.runner.workspace_root.clone())
                .unwrap_or_else(|| ".".to_string()),
        )?;
        (exit_code == 0).then_some(()).ok_or_else(|| {
            invalid(
                "runtime_materialization",
                "remote runtime materialization command failed",
            )
        })
    }

    fn remote_archive_snapshot(&self, source: &Path, destination: &Path) -> Result<()> {
        let temp = tempfile::tempdir().map_err(io("create runtime snapshot directory"))?;
        let snapshot = temp.path().join("snapshot");
        copy_snapshot_to_directory(source, &snapshot, &[])?;
        let archive = temp.path().join("runtime.tar.gz");
        let status = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .args(["-C"])
            .arg(&snapshot)
            .arg(".")
            .status()
            .map_err(io("archive runtime snapshot"))?;
        if !status.success() {
            return Err(invalid(
                "runtime_sources",
                "could not archive runtime snapshot",
            ));
        }
        let archive_path = destination.with_extension("runtime.tar.gz");
        let parent = destination.parent().ok_or_else(|| {
            invalid(
                "runtime_sources.destination_path",
                "runtime destination has no parent",
            )
        })?;
        self.transport
            .ensure_directory(&self.runner, &parent.display().to_string())?;
        self.transport.upload_file(
            &self.runner,
            &archive.display().to_string(),
            &archive_path.display().to_string(),
        )?;
        self.remote_exec(
            vec![
                "mkdir".to_string(),
                "-p".to_string(),
                destination.display().to_string(),
            ],
            None,
        )?;
        let unpack = self.remote_exec(
            vec![
                "tar".to_string(),
                "-xzf".to_string(),
                archive_path.display().to_string(),
                "-C".to_string(),
                destination.display().to_string(),
            ],
            None,
        );
        let remove = self.remote_exec(
            vec![
                "rm".to_string(),
                "-f".to_string(),
                archive_path.display().to_string(),
            ],
            None,
        );
        unpack.and(remove)
    }
}

impl<T: RunnerRuntimeMaterializerTransport> AgentRuntimeMaterializerOperations
    for RunnerRuntimeMaterializerOperations<T>
{
    fn generation_root(&mut self, runner: &Runner, generation_identity: &str) -> Result<PathBuf> {
        let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
            invalid(
                "runner.workspace_root",
                "runtime materialization requires runner.workspace_root",
            )
        })?;
        Ok(Path::new(workspace_root)
            .join("agent-runtime-generations")
            .join(generation_path_component(generation_identity)?))
    }

    fn source_revision(&mut self, source: &Path) -> Result<String> {
        homeboy_core::git::head_sha(source).ok_or_else(|| {
            invalid(
                "runtime_sources",
                "runtime source has no immutable git revision",
            )
        })
    }

    fn source_exists(&mut self, source: &Path) -> Result<bool> {
        Ok(source.exists())
    }

    fn snapshot_source(&mut self, source: &Path, destination: &Path) -> Result<()> {
        // A local source is never a path contract for a remote runner. Snapshot
        // it first, then transfer one immutable archive into the runner staging
        // root through the selected transport.
        if self.runner.kind == RunnerKind::Local {
            copy_snapshot_to_directory(source, destination, &[])
        } else {
            self.remote_archive_snapshot(source, destination)
        }
    }

    fn create_dir_all(&mut self, path: &Path) -> Result<()> {
        if self.runner.kind == RunnerKind::Local {
            fs::create_dir_all(path).map_err(io("create runtime generation directory"))
        } else {
            self.remote_exec(
                vec![
                    "mkdir".to_string(),
                    "-p".to_string(),
                    path.display().to_string(),
                ],
                None,
            )
        }
    }

    fn path_exists(&mut self, path: &Path) -> Result<bool> {
        if self.runner.kind == RunnerKind::Local {
            Ok(path.exists())
        } else {
            Ok(self
                .remote_exec(
                    vec![
                        "test".to_string(),
                        "-e".to_string(),
                        path.display().to_string(),
                    ],
                    None,
                )
                .is_ok())
        }
    }

    fn read_record(&mut self, root: &Path) -> Result<Option<ResolvedAgentRuntimeGeneration>> {
        if self.runner.kind != RunnerKind::Local {
            let temp =
                tempfile::NamedTempFile::new().map_err(io("create runtime record download"))?;
            let record = root.join(RECORD_FILE);
            if !self.path_exists(&record)? {
                return Ok(None);
            }
            self.transport.download_file(
                &self.runner,
                &record.display().to_string(),
                &temp.path().display().to_string(),
            )?;
            let bytes =
                fs::read(temp.path()).map_err(io("read downloaded runtime generation record"))?;
            return serde_json::from_slice(&bytes).map(Some).map_err(|error| {
                Error::validation_invalid_json(
                    error,
                    Some("parse runtime generation record".to_string()),
                    None,
                )
            });
        }
        let path = root.join(RECORD_FILE);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
                Error::validation_invalid_json(
                    error,
                    Some("parse runtime generation record".to_string()),
                    None,
                )
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io("read runtime generation record")(error)),
        }
    }

    fn write_record(
        &mut self,
        root: &Path,
        generation: &ResolvedAgentRuntimeGeneration,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(generation).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize runtime generation record".to_string()),
            )
        })?;
        if self.runner.kind == RunnerKind::Local {
            fs::write(root.join(RECORD_FILE), bytes).map_err(io("write runtime generation record"))
        } else {
            let temp =
                tempfile::NamedTempFile::new().map_err(io("create runtime record upload"))?;
            fs::write(temp.path(), bytes).map_err(io("write runtime generation record"))?;
            self.transport.upload_file(
                &self.runner,
                &temp.path().display().to_string(),
                &root.join(RECORD_FILE).display().to_string(),
            )
        }
    }

    fn run_argv(&mut self, runner: &Runner, argv: &[String], cwd: &Path) -> Result<()> {
        let exit_code = self
            .transport
            .exec(runner, argv.to_vec(), cwd.display().to_string())?;
        if exit_code == 0 {
            Ok(())
        } else {
            Err(invalid(
                "preparation.argv",
                &format!("runtime preparation exited with status {exit_code}"),
            ))
        }
    }

    fn rename_publish(&mut self, staging: &Path, destination: &Path) -> Result<()> {
        if self.runner.kind == RunnerKind::Local {
            fs::rename(staging, destination).map_err(io("publish runtime generation"))
        } else {
            self.remote_exec(
                vec![
                    "mv".to_string(),
                    staging.display().to_string(),
                    destination.display().to_string(),
                ],
                None,
            )
        }
    }

    fn remove_tree(&mut self, path: &Path) -> Result<()> {
        if self.runner.kind != RunnerKind::Local {
            return self.remote_exec(
                vec![
                    "rm".to_string(),
                    "-rf".to_string(),
                    path.display().to_string(),
                ],
                None,
            );
        }
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io("remove runtime generation staging directory")(error)),
        }
    }
}

/// Materialize one exact generation. The process-global guard closes the
/// same-process race; runtime-promotion serializes competing controller
/// processes before any staging root is created.
pub fn materialize_agent_runtime_generation(
    operations: &mut impl AgentRuntimeMaterializerOperations,
    plan: &AgentRuntimeMaterializationPlan,
    runner: &Runner,
    _job_or_run_id: &str,
) -> Result<ResolvedAgentRuntimeGeneration> {
    validate_plan_for_materialization(plan)?;
    let _promotion = homeboy_core::runtime_promotion::acquire(
        "agent runtime materialization",
        format!("{}:{}", runner.id, plan.generation_identity),
    )?;
    materialize_generation_with_operations(operations, plan, runner)
}

fn materialize_generation_with_operations(
    operations: &mut impl AgentRuntimeMaterializerOperations,
    plan: &AgentRuntimeMaterializationPlan,
    runner: &Runner,
) -> Result<ResolvedAgentRuntimeGeneration> {
    let _same_process = generation_lock()
        .lock()
        .expect("runtime generation lock poisoned");
    let root = operations.generation_root(runner, &plan.generation_identity)?;
    if let Some(existing) = operations.read_record(&root)? {
        if generation_matches(plan, &root, &existing, operations)? {
            return Ok(ResolvedAgentRuntimeGeneration {
                publication: AgentRuntimeGenerationPublication::Reused,
                cleanup: AgentRuntimeGenerationCleanup::Immutable,
                ..existing
            });
        }
        return Err(invalid(
            "generation_identity",
            "existing runtime generation is incompatible or invalid",
        ));
    }

    let staging = staging_root(&root)?;
    operations.remove_tree(&staging)?;
    let result = materialize_staging(operations, plan, runner, &root, &staging);
    match result {
        Ok(generation) => {
            if let Err(error) = operations.rename_publish(&staging, &root) {
                let _ = operations.remove_tree(&staging);
                return Err(error);
            }
            Ok(generation)
        }
        Err(error) => {
            let _ = operations.remove_tree(&staging);
            Err(error)
        }
    }
}

fn materialize_staging(
    operations: &mut impl AgentRuntimeMaterializerOperations,
    plan: &AgentRuntimeMaterializationPlan,
    runner: &Runner,
    root: &Path,
    staging: &Path,
) -> Result<ResolvedAgentRuntimeGeneration> {
    operations.create_dir_all(staging)?;
    let mut sources = Vec::with_capacity(plan.runtime_sources.len());
    for source in &plan.runtime_sources {
        let destination = safe_join(staging, &source.destination_path)?;
        let resolved = match &source.locator {
            AgentRuntimeSourceLocator::LocalPath { path } => {
                let source_path = Path::new(path);
                if !operations.source_exists(source_path)? {
                    return Err(invalid(
                        "runtime_sources.locator.path",
                        "runtime source is unavailable",
                    ));
                }
                let resolved = operations.source_revision(source_path)?;
                if resolved != source.content_identity {
                    return Err(invalid(
                        "runtime_sources.content_identity",
                        "runtime source revision does not match the requested content identity",
                    ));
                }
                operations.snapshot_source(source_path, &destination)?;
                resolved
            }
            AgentRuntimeSourceLocator::Git {
                remote_url,
                revision,
            } => {
                // `revision` is validated as the source content identity before
                // this transaction. Every remote operation is argv-only and the
                // detached checkout proves the fetched object is that identity.
                operations.run_argv(
                    runner,
                    &[
                        "git".to_string(),
                        "clone".to_string(),
                        "--no-checkout".to_string(),
                        remote_url.clone(),
                        destination.display().to_string(),
                    ],
                    staging,
                )?;
                operations.run_argv(
                    runner,
                    &[
                        "git".to_string(),
                        "-C".to_string(),
                        destination.display().to_string(),
                        "fetch".to_string(),
                        "--depth".to_string(),
                        "1".to_string(),
                        "origin".to_string(),
                        revision.clone(),
                    ],
                    staging,
                )?;
                operations.run_argv(
                    runner,
                    &[
                        "git".to_string(),
                        "-C".to_string(),
                        destination.display().to_string(),
                        "checkout".to_string(),
                        "--detach".to_string(),
                        revision.clone(),
                    ],
                    staging,
                )?;
                revision.clone()
            }
        };
        sources.push(ResolvedAgentRuntimeSourceIdentity {
            id: source.id.clone(),
            requested_content_identity: source.content_identity.clone(),
            resolved_content_identity: resolved,
            destination_path: source.destination_path.clone(),
        });
    }
    for action in &plan.preparation {
        let cwd = safe_join(staging, &action.cwd)?;
        if !operations.path_exists(&cwd)? {
            return Err(invalid(
                "preparation.cwd",
                "runtime preparation cwd does not exist",
            ));
        }
        operations.run_argv(runner, &action.argv, &cwd)?;
        for output in &action.expected_outputs {
            if !operations.path_exists(&safe_join(staging, output)?)? {
                return Err(invalid(
                    "preparation.expected_outputs",
                    "runtime preparation did not produce an expected output",
                ));
            }
        }
    }
    let runtime_destination = plan.runtime_sources[0].destination_path.clone();
    let runtime_path = safe_join(staging, &runtime_destination)?;
    if !operations.path_exists(&runtime_path)? {
        return Err(invalid(
            "runtime_path",
            "materialized runtime path is missing",
        ));
    }
    let generation = ResolvedAgentRuntimeGeneration {
        generation_identity: plan.generation_identity.clone(),
        content_identity: plan.generation_identity.clone(),
        immutable_root: root.display().to_string(),
        runtime_path: root.join(runtime_destination).display().to_string(),
        provider_id: plan.provider_id.clone(),
        requested_runtime_id: plan.runtime_id.clone(),
        requested_source_revision: plan.source_revision.clone(),
        resolved_source_identities: sources,
        publication: AgentRuntimeGenerationPublication::Published,
        cleanup: AgentRuntimeGenerationCleanup::Immutable,
    };
    operations.write_record(staging, &generation)?;
    Ok(generation)
}

fn generation_matches(
    plan: &AgentRuntimeMaterializationPlan,
    root: &Path,
    generation: &ResolvedAgentRuntimeGeneration,
    operations: &mut impl AgentRuntimeMaterializerOperations,
) -> Result<bool> {
    let sources_match = generation
        .resolved_source_identities
        .iter()
        .zip(&plan.runtime_sources)
        .all(|(resolved, requested)| {
            resolved.id == requested.id
                && resolved.requested_content_identity == requested.content_identity
                && resolved.resolved_content_identity == requested.content_identity
                && resolved.destination_path == requested.destination_path
        });
    Ok(generation.generation_identity == plan.generation_identity
        && generation.provider_id == plan.provider_id
        && generation.requested_runtime_id == plan.runtime_id
        && generation.resolved_source_identities.len() == plan.runtime_sources.len()
        && sources_match
        && operations.path_exists(root)?
        && operations.path_exists(Path::new(&generation.runtime_path))?)
}

fn validate_plan_for_materialization(plan: &AgentRuntimeMaterializationPlan) -> Result<()> {
    validate_runtime_materialization_plan(plan)?;
    if plan.generation_identity.trim().is_empty() || plan.runtime_sources.is_empty() {
        return Err(invalid(
            "generation_identity",
            "runtime materialization requires a generation identity and source",
        ));
    }
    for action in &plan.preparation {
        if action
            .argv
            .iter()
            .any(|arg| arg.contains('\n') || arg.contains('\r'))
        {
            return Err(invalid(
                "preparation.argv",
                "runtime preparation argv cannot contain line breaks",
            ));
        }
    }
    Ok(())
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    if relative.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid(
            "runtime materialization path",
            "path must be a non-empty relative path without traversal",
        ));
    }
    Ok(root.join(path))
}

fn generation_path_component(identity: &str) -> Result<&str> {
    let component = identity.strip_prefix("sha256:").unwrap_or(identity);
    if component.len() != 64 || !component.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid(
            "generation_identity",
            "generation identity must be a sha256 content address",
        ));
    }
    Ok(component)
}

fn staging_root(root: &Path) -> Result<PathBuf> {
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            invalid(
                "generation_identity",
                "generation path has no valid file name",
            )
        })?;
    Ok(root.with_file_name(format!(".{name}{STAGING_SUFFIX}")))
}

fn generation_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn invalid(field: &str, message: &str) -> Error {
    Error::validation_invalid_argument(field, message, None, None)
}

fn io(context: &'static str) -> impl FnOnce(std::io::Error) -> Error {
    move |error| Error::internal_io(error.to_string(), Some(context.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::thread;

    use homeboy_core::agent_runtime_manifest::{
        AgentRuntimeMaterializationPlan, AgentRuntimeMaterializationSource,
        AgentRuntimePreparationAction,
    };
    use homeboy_core::server::{RunnerPolicy, RunnerSettings};

    fn evidence_generation() -> ResolvedAgentRuntimeGeneration {
        ResolvedAgentRuntimeGeneration {
            generation_identity: "a".repeat(64),
            content_identity: "b".repeat(64),
            immutable_root: "/runner/generation".to_string(),
            runtime_path: "/runner/generation/runtime".to_string(),
            provider_id: "provider-a".to_string(),
            requested_runtime_id: "runtime-a".to_string(),
            requested_source_revision: Some("c".repeat(40)),
            resolved_source_identities: Vec::new(),
            publication: AgentRuntimeGenerationPublication::Published,
            cleanup: AgentRuntimeGenerationCleanup::Immutable,
        }
    }

    fn evidence_args(generation: &ResolvedAgentRuntimeGeneration) -> Vec<String> {
        let plan = AgentRuntimeMaterializationPlan {
            schema: "homeboy/agent-runtime-materialization-plan/v2".to_string(),
            runtime_id: generation.requested_runtime_id.clone(),
            selected_identity: Default::default(),
            provider_id: generation.provider_id.clone(),
            source_selector: "test".to_string(),
            source_revision: generation.requested_source_revision.clone(),
            freshness: Default::default(),
            runtime_path: Some(generation.runtime_path.clone()),
            runtime_sources: Vec::new(),
            preparation: Vec::new(),
            generation_identity: generation.generation_identity.clone(),
            source_roots: Vec::new(),
            dependencies: Vec::new(),
            executable_requirements: Vec::new(),
            readiness_checks: Vec::new(),
            env_passthrough: Vec::new(),
            workspace: None,
        };
        let policy = ResolvedAgentTaskProviderPolicy {
            backend: "test".to_string(),
            selector: None,
            model: None,
            rotation: None,
            rotation_starts_with_first_entry: false,
            retry: Default::default(),
            liveness_timeout_ms: None,
            runtime_identity: Some(homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity {
                runtime_id: generation.requested_runtime_id.clone(),
                provider_id: generation.provider_id.clone(),
                source_selector: "test".to_string(),
                source_revision: generation.requested_source_revision.clone().unwrap(),
                freshness: homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeFreshness::Unverifiable,
                provider: serde_json::Value::Null,
                materialization_plan: serde_json::to_value(plan).unwrap(),
            }),
        };
        vec![format!(
            "--resolved-provider-policy={}",
            serde_json::to_string(&policy).unwrap()
        )]
    }

    #[test]
    fn runtime_evidence_roundtrips_and_rejects_mismatch_before_submission() {
        let generation = evidence_generation();
        let args = evidence_args(&generation);
        let evidence =
            runtime_execution_evidence(&generation, &args, "runner-a").expect("evidence");
        let restored: AgentRuntimeExecutionEvidence =
            serde_json::from_value(serde_json::to_value(&evidence).unwrap()).expect("roundtrip");
        assert_eq!(restored, evidence);

        let mut mismatched = evidence_args(&generation);
        mismatched[0] = mismatched[0].replace(&generation.runtime_path, "/wrong/runtime");
        assert!(runtime_execution_evidence(&generation, &mismatched, "runner-a").is_err());
    }

    #[derive(Clone, Default)]
    struct RecorderTransport {
        events: Arc<Mutex<Vec<String>>>,
        fail_preparation: bool,
    }

    impl RecorderTransport {
        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    impl RunnerRuntimeMaterializerTransport for RecorderTransport {
        fn exec(&self, _: &Runner, argv: Vec<String>, cwd: String) -> Result<i32> {
            self.events
                .lock()
                .unwrap()
                .push(format!("exec cwd={cwd} argv={}", argv.join("|")));
            let missing_record = argv.first().is_some_and(|command| command == "test")
                && argv.last().is_some_and(|path| path.ends_with(RECORD_FILE));
            let failed_preparation =
                self.fail_preparation && argv.first().is_some_and(|command| command == "prepare");
            Ok(i32::from(missing_record || failed_preparation))
        }

        fn ensure_directory(&self, _: &Runner, path: &str) -> Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("ensure-directory {path}"));
            Ok(())
        }

        fn upload_file(&self, _: &Runner, local_path: &str, remote_path: &str) -> Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("upload {local_path} -> {remote_path}"));
            Ok(())
        }

        fn download_file(&self, _: &Runner, remote_path: &str, local_path: &str) -> Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("download {remote_path} -> {local_path}"));
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeOperations {
        _tempdir: Arc<tempfile::TempDir>,
        base: Arc<PathBuf>,
        revisions: Arc<HashMap<PathBuf, String>>,
        fail_prepare: bool,
        snapshots: Arc<Mutex<usize>>,
    }

    impl AgentRuntimeMaterializerOperations for FakeOperations {
        fn generation_root(&mut self, _: &Runner, identity: &str) -> Result<PathBuf> {
            Ok(self
                .base
                .join("generations")
                .join(generation_path_component(identity)?))
        }
        fn source_exists(&mut self, source: &Path) -> Result<bool> {
            Ok(self.revisions.contains_key(source))
        }
        fn source_revision(&mut self, source: &Path) -> Result<String> {
            self.revisions
                .get(source)
                .cloned()
                .ok_or_else(|| invalid("source", "missing source"))
        }
        fn snapshot_source(&mut self, _: &Path, destination: &Path) -> Result<()> {
            *self.snapshots.lock().unwrap() += 1;
            fs::create_dir_all(destination).map_err(io("fake snapshot"))
        }
        fn create_dir_all(&mut self, path: &Path) -> Result<()> {
            fs::create_dir_all(path).map_err(io("fake mkdir"))
        }
        fn path_exists(&mut self, path: &Path) -> Result<bool> {
            Ok(path.exists() || self.revisions.contains_key(path))
        }
        fn read_record(&mut self, root: &Path) -> Result<Option<ResolvedAgentRuntimeGeneration>> {
            match fs::read(root.join(RECORD_FILE)) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|error| Error::internal_json(error.to_string(), None)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(io("fake record")(error)),
            }
        }
        fn write_record(
            &mut self,
            root: &Path,
            generation: &ResolvedAgentRuntimeGeneration,
        ) -> Result<()> {
            fs::write(
                root.join(RECORD_FILE),
                serde_json::to_vec(generation).unwrap(),
            )
            .map_err(io("fake record"))
        }
        fn run_argv(&mut self, _: &Runner, _: &[String], _: &Path) -> Result<()> {
            if self.fail_prepare {
                Err(invalid("preparation", "forced preparation failure"))
            } else {
                Ok(())
            }
        }
        fn rename_publish(&mut self, staging: &Path, destination: &Path) -> Result<()> {
            fs::rename(staging, destination).map_err(io("fake publish"))
        }
        fn remove_tree(&mut self, path: &Path) -> Result<()> {
            match fs::remove_dir_all(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(io("fake remove")(error)),
            }
        }
    }

    // The production entrypoint also owns a machine-global promotion lease.
    // Unit tests exercise the engine's generation-scoped lock directly so they
    // remain isolated from unrelated parallel runner lease tests.
    fn materialize_agent_runtime_generation(
        operations: &mut impl AgentRuntimeMaterializerOperations,
        plan: &AgentRuntimeMaterializationPlan,
        runner: &Runner,
        _: &str,
    ) -> Result<ResolvedAgentRuntimeGeneration> {
        validate_plan_for_materialization(plan)?;
        materialize_generation_with_operations(operations, plan, runner)
    }

    #[test]
    fn compatible_generation_is_a_no_op() {
        let (mut operations, runner, plan) = fixture("a", false);
        let first =
            materialize_agent_runtime_generation(&mut operations, &plan, &runner, "run-a").unwrap();
        let second =
            materialize_agent_runtime_generation(&mut operations, &plan, &runner, "run-b").unwrap();
        assert_eq!(
            first.publication,
            AgentRuntimeGenerationPublication::Published
        );
        assert_eq!(
            second.publication,
            AgentRuntimeGenerationPublication::Reused
        );
        assert_eq!(*operations.snapshots.lock().unwrap(), 1);
    }

    #[test]
    fn stale_generation_materializes_and_publishes() {
        let (mut operations, runner, plan) = fixture("b", false);
        let generation =
            materialize_agent_runtime_generation(&mut operations, &plan, &runner, "run").unwrap();
        assert_eq!(
            generation.publication,
            AgentRuntimeGenerationPublication::Published
        );
        assert!(Path::new(&generation.runtime_path).is_dir());
        assert!(Path::new(&generation.immutable_root)
            .join(RECORD_FILE)
            .is_file());
    }

    #[test]
    fn preparation_failure_removes_staging_and_publishes_nothing() {
        let (mut operations, runner, plan) = fixture("c", true);
        let root = operations
            .generation_root(&runner, &plan.generation_identity)
            .unwrap();
        assert!(
            materialize_agent_runtime_generation(&mut operations, &plan, &runner, "run").is_err()
        );
        assert!(!root.exists());
        assert!(!staging_root(&root).unwrap().exists());
    }

    #[test]
    fn unavailable_source_rejects_without_publication() {
        let (mut operations, runner, mut plan) = fixture("d", false);
        plan.runtime_sources[0].locator = AgentRuntimeSourceLocator::LocalPath {
            path: operations.base.join("gone").display().to_string(),
        };
        let root = operations
            .generation_root(&runner, &plan.generation_identity)
            .unwrap();
        assert!(
            materialize_agent_runtime_generation(&mut operations, &plan, &runner, "run").is_err()
        );
        assert!(!root.exists());
    }

    #[test]
    fn concurrent_identical_requirements_dedupe() {
        let (operations, runner, plan) = fixture("e", false);
        let left = {
            let mut operations = operations.clone();
            let runner = runner.clone();
            let plan = plan.clone();
            thread::spawn(move || {
                materialize_agent_runtime_generation(&mut operations, &plan, &runner, "one")
            })
        };
        let right = {
            let mut operations = operations.clone();
            let runner = runner.clone();
            thread::spawn(move || {
                materialize_agent_runtime_generation(&mut operations, &plan, &runner, "two")
            })
        };
        let publications = [
            left.join().unwrap().unwrap().publication,
            right.join().unwrap().unwrap().publication,
        ];
        assert!(publications.contains(&AgentRuntimeGenerationPublication::Published));
        assert!(publications.contains(&AgentRuntimeGenerationPublication::Reused));
        assert_eq!(*operations.snapshots.lock().unwrap(), 1);
    }

    #[test]
    fn concurrent_distinct_revisions_use_isolated_roots() {
        let (operations, runner, left_plan) = fixture("f", false);
        let mut right_plan = left_plan.clone();
        right_plan.generation_identity = format!("sha256:{}", "b".repeat(64));
        right_plan.runtime_sources[0].content_identity = "b".repeat(40);
        let source = match &right_plan.runtime_sources[0].locator {
            AgentRuntimeSourceLocator::LocalPath { path } => PathBuf::from(path),
            _ => unreachable!(),
        };
        let mut right_operations = operations.clone();
        Arc::make_mut(&mut right_operations.revisions).insert(source, "b".repeat(40));
        let left = {
            let mut operations = operations.clone();
            let runner = runner.clone();
            thread::spawn(move || {
                materialize_agent_runtime_generation(&mut operations, &left_plan, &runner, "one")
            })
        };
        let right = {
            let runner = runner.clone();
            thread::spawn(move || {
                materialize_agent_runtime_generation(
                    &mut right_operations,
                    &right_plan,
                    &runner,
                    "two",
                )
            })
        };
        let left = left.join().unwrap().unwrap();
        let right = right.join().unwrap().unwrap();
        assert_ne!(left.immutable_root, right.immutable_root);
        assert!(Path::new(&left.immutable_root).is_dir());
        assert!(Path::new(&right.immutable_root).is_dir());
    }

    #[test]
    fn rejects_traversal_and_unsafe_argv() {
        let (mut operations, runner, mut plan) = fixture("g", false);
        plan.runtime_sources[0].destination_path = "../escape".to_string();
        assert!(
            materialize_agent_runtime_generation(&mut operations, &plan, &runner, "run").is_err()
        );
        let (_, _, mut plan) = fixture("h", false);
        plan.preparation[0].argv = vec!["tool\nargument".to_string()];
        assert!(validate_plan_for_materialization(&plan).is_err());
    }

    #[test]
    fn ssh_production_adapter_plans_snapshot_preparation_checks_and_atomic_publish() {
        let (tempdir, runner, plan) = ssh_fixture("a");
        let recorder = RecorderTransport::default();
        let mut operations =
            RunnerRuntimeMaterializerOperations::with_transport(runner.clone(), recorder.clone());

        let generation =
            materialize_agent_runtime_generation(&mut operations, &plan, &runner, "run-ssh")
                .expect("materialize SSH runtime");
        let events = recorder.events();
        let root = format!(
            "/runner/workspace/agent-runtime-generations/{}",
            generation_path_component(&plan.generation_identity).unwrap()
        );

        assert!(events
            .iter()
            .any(|event| event.starts_with("ensure-directory ")));
        assert!(events.iter().any(|event| event.contains("upload ")));
        assert!(events
            .iter()
            .any(|event| event.contains("argv=mkdir|-p") && event.contains(".staging/runtime")));
        assert!(events
            .iter()
            .any(|event| event.contains("argv=tar|-xzf") && event.contains(".staging/runtime")));
        assert!(events.iter().any(|event| event
            .contains("exec cwd=/runner/workspace/agent-runtime-generations")
            && event.contains("argv=prepare")));
        assert!(events
            .iter()
            .any(|event| event.contains("argv=test|-e") && event.contains(".staging/runtime")));
        assert!(
            events
                .iter()
                .any(|event| event.contains("argv=mv|") && event.contains(&root)),
            "expected atomic publish for {root}; events: {events:?}"
        );
        assert_eq!(generation.immutable_root, root);
        drop(tempdir);
    }

    #[test]
    fn ssh_production_adapter_rejects_unavailable_source_before_transfer_or_preparation() {
        let (tempdir, runner, mut plan) = ssh_fixture("b");
        plan.runtime_sources[0].locator = AgentRuntimeSourceLocator::LocalPath {
            path: tempdir.path().join("missing").display().to_string(),
        };
        let recorder = RecorderTransport::default();
        let mut operations =
            RunnerRuntimeMaterializerOperations::with_transport(runner.clone(), recorder.clone());

        assert!(
            materialize_agent_runtime_generation(&mut operations, &plan, &runner, "run-ssh")
                .is_err()
        );
        let events = recorder.events();
        assert!(!events.iter().any(|event| event.starts_with("upload ")));
        assert!(!events.iter().any(|event| event.contains("argv=prepare")));
        assert!(!events.iter().any(|event| event.contains("argv=mv|")));
    }

    #[test]
    fn git_source_materializes_with_immutable_argv_checkout() {
        let (tempdir, runner, mut plan) = ssh_fixture("a");
        let revision = plan.runtime_sources[0].content_identity.clone();
        plan.runtime_sources[0].locator = AgentRuntimeSourceLocator::Git {
            remote_url: "https://example.test/runtime.git".to_string(),
            revision: revision.clone(),
        };
        let recorder = RecorderTransport::default();
        let mut operations =
            RunnerRuntimeMaterializerOperations::with_transport(runner.clone(), recorder.clone());

        let generation =
            materialize_agent_runtime_generation(&mut operations, &plan, &runner, "run-ssh")
                .expect("materialize immutable git runtime");
        let events = recorder.events();

        assert!(events.iter().any(|event| {
            event.contains("argv=git|clone|--no-checkout|https://example.test/runtime.git")
        }));
        assert!(events.iter().any(|event| {
            event.contains("argv=git|-C")
                && event.contains("fetch|--depth|1|origin")
                && event.contains(&revision)
        }));
        assert!(events.iter().any(|event| {
            event.contains("argv=git|-C")
                && event.contains("checkout|--detach")
                && event.contains(&revision)
        }));
        assert_eq!(
            generation.resolved_source_identities[0].resolved_content_identity,
            revision
        );
        drop(tempdir);
    }

    #[test]
    fn ssh_production_adapter_removes_staging_without_publish_when_preparation_fails() {
        let (tempdir, runner, plan) = ssh_fixture("c");
        let recorder = RecorderTransport {
            fail_preparation: true,
            ..Default::default()
        };
        let mut operations =
            RunnerRuntimeMaterializerOperations::with_transport(runner.clone(), recorder.clone());

        assert!(
            materialize_agent_runtime_generation(&mut operations, &plan, &runner, "run-ssh")
                .is_err()
        );
        let events = recorder.events();
        assert!(events
            .iter()
            .any(|event| event.contains("argv=rm|-rf") && event.contains(".staging")));
        assert!(!events.iter().any(|event| event.contains("argv=mv|")));
        drop(tempdir);
    }

    fn ssh_fixture(seed: &str) -> (tempfile::TempDir, Runner, AgentRuntimeMaterializationPlan) {
        let tempdir = tempfile::tempdir().unwrap();
        let source = tempdir.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("runtime.txt"), "runtime").unwrap();
        for args in [
            &["init"][..],
            &["add", "."][..],
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "runtime",
            ][..],
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .unwrap();
            assert!(status.success());
        }
        let revision = homeboy_core::git::head_sha(&source).unwrap();
        let runner = Runner {
            id: format!("runner-ssh-{seed}"),
            kind: RunnerKind::Ssh,
            server_id: Some("server-ssh".to_string()),
            workspace_root: Some("/runner/workspace".to_string()),
            settings: RunnerSettings::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: RunnerPolicy::default(),
        };
        let plan = AgentRuntimeMaterializationPlan {
            schema: "homeboy/agent-runtime-materialization-plan/v2".to_string(),
            runtime_id: "runtime".to_string(),
            selected_identity: Default::default(),
            provider_id: "provider".to_string(),
            source_selector: "test".to_string(),
            source_revision: Some(revision.clone()),
            freshness: Default::default(),
            runtime_path: None,
            runtime_sources: vec![AgentRuntimeMaterializationSource {
                id: "runtime".to_string(),
                locator: AgentRuntimeSourceLocator::LocalPath {
                    path: source.display().to_string(),
                },
                content_identity: revision,
                destination_path: "runtime".to_string(),
            }],
            preparation: vec![AgentRuntimePreparationAction {
                argv: vec!["prepare".to_string()],
                cwd: "runtime".to_string(),
                expected_outputs: vec!["runtime".to_string()],
                runtime_identity: None,
            }],
            generation_identity: format!("sha256:{}", seed.repeat(64)[..64].to_string()),
            source_roots: vec![],
            dependencies: vec![],
            executable_requirements: vec![],
            readiness_checks: vec![],
            env_passthrough: vec![],
            workspace: None,
        };
        (tempdir, runner, plan)
    }

    fn fixture(
        seed: &str,
        fail_prepare: bool,
    ) -> (FakeOperations, Runner, AgentRuntimeMaterializationPlan) {
        let tempdir = Arc::new(tempfile::tempdir().unwrap());
        let base = Arc::new(tempdir.path().to_path_buf());
        let source = base.join("source");
        fs::create_dir_all(&source).unwrap();
        let revision = format!("{seed:0<40}");
        let source_path = source.display().to_string();
        let operations = FakeOperations {
            _tempdir: tempdir,
            base: base.clone(),
            revisions: Arc::new(HashMap::from([(source.clone(), revision.clone())])),
            fail_prepare,
            snapshots: Arc::new(Mutex::new(0)),
        };
        let runner = Runner {
            id: format!("runner-{seed}"),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: Some(base.display().to_string()),
            settings: RunnerSettings::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: RunnerPolicy::default(),
        };
        let plan = AgentRuntimeMaterializationPlan {
            schema: "homeboy/agent-runtime-materialization-plan/v2".to_string(),
            runtime_id: "runtime".to_string(),
            selected_identity: Default::default(),
            provider_id: "provider".to_string(),
            source_selector: "test".to_string(),
            source_revision: Some(revision.clone()),
            freshness: Default::default(),
            runtime_path: None,
            runtime_sources: vec![AgentRuntimeMaterializationSource {
                id: "runtime".to_string(),
                locator: AgentRuntimeSourceLocator::LocalPath { path: source_path },
                content_identity: revision,
                destination_path: "runtime".to_string(),
            }],
            preparation: vec![AgentRuntimePreparationAction {
                argv: vec!["prepare".to_string()],
                cwd: "runtime".to_string(),
                expected_outputs: vec![],
                runtime_identity: None,
            }],
            generation_identity: format!("sha256:{}", seed.repeat(64)[..64].to_string()),
            source_roots: vec![],
            dependencies: vec![],
            executable_requirements: vec![],
            readiness_checks: vec![],
            env_passthrough: vec![],
            workspace: None,
        };
        (operations, runner, plan)
    }
}
