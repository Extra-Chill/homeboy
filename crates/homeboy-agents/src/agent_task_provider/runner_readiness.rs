use super::*;

pub(super) fn provider_executable_env(
    provider: &AgentTaskExecutorProvider,
) -> Result<Vec<(String, String)>, AgentTaskProviderExecutableResolutionError> {
    let mut env = Vec::new();
    for readiness in &provider.runner_readiness {
        let Some(executable) = readiness.executable.as_ref() else {
            continue;
        };
        let resolved = resolve_provider_executable(readiness, executable)?;
        for name in resolved.env {
            env.push((name, resolved.path.clone()));
        }
    }
    Ok(env)
}

fn resolve_provider_executable(
    readiness: &AgentTaskProviderRunnerReadiness,
    executable: &AgentTaskProviderExecutableReadiness,
) -> Result<AgentTaskProviderResolvedExecutable, AgentTaskProviderExecutableResolutionError> {
    let env_names: Vec<String> = executable
        .env
        .iter()
        .filter(|name| !name.trim().is_empty())
        .cloned()
        .collect();
    for name in &env_names {
        if let Ok(value) = std::env::var(name) {
            let value = value.trim();
            if !value.is_empty() {
                return Ok(AgentTaskProviderResolvedExecutable {
                    env: env_names,
                    path: value.to_string(),
                });
            }
        }
    }

    for candidate in &executable.candidates {
        if let Some(path) = resolve_executable_candidate(candidate) {
            return Ok(AgentTaskProviderResolvedExecutable {
                env: env_names,
                path,
            });
        }
    }

    Err(AgentTaskProviderExecutableResolutionError {
        readiness_id: readiness.id.clone(),
        label: readiness.label.clone(),
        env: executable.env.clone(),
        candidates: executable.candidates.clone(),
        install_hint: executable
            .install_hint
            .clone()
            .or_else(|| readiness.remediation.clone()),
    })
}

pub(super) fn resolve_executable_candidate(candidate: &str) -> Option<String> {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return None;
    }
    let candidate_path = Path::new(candidate);
    if candidate_path.components().count() > 1 || candidate_path.is_absolute() {
        return executable_file(candidate_path).then(|| candidate.to_string());
    }
    let path_var = std::env::var_os("PATH")?;
    for path in std::env::split_paths(&path_var) {
        let resolved = path.join(candidate);
        if executable_file(&resolved) {
            return Some(resolved.to_string_lossy().to_string());
        }
    }
    None
}

#[cfg(unix)]
pub(super) fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub(super) fn executable_file(path: &Path) -> bool {
    path.is_file()
}
