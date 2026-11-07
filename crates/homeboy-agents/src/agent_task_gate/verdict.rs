//! Content-addressed gate verdicts on the execution host. Unidentifiable inputs
//! deliberately miss: a cache must never broaden a gate's authority.
#[cfg(test)]
mod tests;
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateVerdictEvidence {
    pub key: String,
    pub original_run: String,
    pub reused: bool,
    pub elapsed_ms: u128,
    pub saved_ms: u128,
}

pub(super) struct GateVerdict {
    key: String,
    tree: String,
    root: PathBuf,
}

fn tree(cwd: &Path) -> Option<String> {
    match crate::agent_task_promotion::candidate_fingerprint(cwd.to_str()?).ok()? {
        crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } => {
            Some(fingerprint.tree)
        }
        _ => None,
    }
}

impl GateVerdict {
    pub(super) fn prepare(
        cwd: &Path,
        process: &Command,
        policy: &AgentTaskGateEnvironmentPolicy,
        environment: &AgentTaskGateEnvironment,
        runtime_tmpdir: Option<&Path>,
        plan: Option<&homeboy_engine_primitives::test_execution::TestExecutionPlan>,
    ) -> Option<Self> {
        // Linked ambient directories have no recorded content identity. Never
        // reuse verdicts across a potentially changed external input.
        if !environment.linked_extensions.is_empty()
            || environment
                .package_artifacts
                .iter()
                .any(|artifact| artifact.artifacts.iter().any(|path| path.sha256.is_none()))
        {
            return None;
        }
        let tree = tree(cwd)?;
        let mut env: BTreeMap<String, String> =
            if policy.mode == AgentTaskGateEnvironmentMode::Inherit {
                std::env::vars().collect()
            } else {
                BTreeMap::new()
            };
        for (name, value) in process.get_envs() {
            let name = name.to_str()?.to_owned();
            if let Some(value) = value {
                env.insert(name, value.to_str()?.to_owned());
            } else {
                env.remove(&name);
            }
        }
        // Scratch locations are execution instances of the same isolated
        // environment, not input identities. Explicit non-isolated paths stay.
        for variable in &environment.sanitized {
            if env.get(&variable.name) == Some(&variable.value) {
                env.insert(
                    variable.name.clone(),
                    format!("<isolated:{}>", variable.name),
                );
            }
        }
        if runtime_tmpdir.is_some() {
            for name in TMPDIR_ENV_VARS {
                env.insert((*name).to_string(), "<gate-temp>".to_string());
            }
        }
        // Input policy, not setup output, defines the environment identity.
        // Unknown external closures deliberately miss before touching setup.
        if !policy.extension_inputs.is_empty() {
            return None;
        }
        let argv: Vec<_> = std::iter::once(process.get_program())
            .chain(process.get_args())
            .map(|arg| arg.to_str())
            .collect::<Option<_>>()?;
        // This locates storage, not a gate input; agent processes carry it
        // through their isolated per-attempt environment.
        env.remove("HOMEBOY_GATE_VERDICT_DATA_ROOT");
        let payload = serde_json::to_vec(&(
            2,
            &tree,
            argv,
            env,
            policy,
            &environment.package_artifacts,
            plan,
        ))
        .ok()?;
        let key = format!("{:x}", Sha256::digest(payload));
        Some(Self {
            key,
            tree,
            root: std::env::var_os("HOMEBOY_GATE_VERDICT_DATA_ROOT")
                .map(PathBuf::from)
                .or_else(|| homeboy_core::paths::homeboy_data().ok())?
                .join("controller-state/gate-verdicts"),
        })
    }

    pub(super) fn passed(
        &self,
        index: usize,
        visibility: AgentTaskGateVisibility,
        reveal: AgentTaskGateRevealPolicy,
    ) -> Option<AgentTaskGateReport> {
        let path = self.root.join(&self.key).join("pass.json");
        let mut report: AgentTaskGateReport =
            serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
        let evidence = report.verdict.as_mut()?;
        if evidence.key != self.key
            || report.status != AgentTaskGateStatus::Succeeded
            || report.exit_code != 0
            || report.workspace_mutation.is_some()
            || report.visibility != visibility
        {
            return None;
        }
        evidence.reused = true;
        evidence.saved_ms = evidence.elapsed_ms;
        // Reconstruct the step and current gate policy instead of carrying the
        // original caller's ordinal or private-output reveal permissions.
        let mut current = AgentTaskGateReport::new(
            format!("gate-{index}"),
            report.command.clone(),
            0,
            report.stdout.clone(),
            report.stderr.clone(),
            None,
            visibility,
            reveal,
            report.environment.clone(),
        );
        report.id = current.id;
        std::mem::swap(&mut report.step, &mut current.step);
        report.visibility = visibility;
        report.reveal_policy = reveal;
        Some(report)
    }

    pub(super) fn record(self, cwd: &Path, report: &mut AgentTaskGateReport, elapsed: Duration) {
        // A command that rewrites its input cannot certify the admitted tree.
        let unchanged = tree(cwd).as_deref() == Some(&self.tree);
        let directory = self.root.join(&self.key);
        if fs::create_dir_all(&directory).is_err() {
            return;
        }
        let Ok(mut file) = tempfile::NamedTempFile::new_in(&directory) else {
            return;
        };
        let original = directory.join(format!("{}.json", uuid::Uuid::new_v4()));
        report.verdict = Some(GateVerdictEvidence {
            key: self.key,
            original_run: original.display().to_string(),
            reused: false,
            elapsed_ms: elapsed.as_millis(),
            saved_ms: 0,
        });
        if serde_json::to_writer(file.as_file_mut(), report).is_err() {
            return;
        }
        if file.persist(&original).is_err() {
            return;
        }
        if !unchanged || report.status != AgentTaskGateStatus::Succeeded || report.exit_code != 0 {
            let _ = fs::remove_file(directory.join("pass.json"));
        } else {
            // Atomic publication: readers never observe a partial verdict.
            if let Ok(mut pass) = tempfile::NamedTempFile::new_in(&directory) {
                if serde_json::to_writer(pass.as_file_mut(), report).is_ok() {
                    let _ = pass.persist(directory.join("pass.json"));
                }
            }
        }
    }
}
