use super::*;
use homeboy_core;
use homeboy_extension_contract::api::v1::{
    ExtensionApiCatalogEntryStatus, ExtensionApiCatalogRequest,
    EXTENSION_API_CATALOG_REQUEST_SCHEMA, EXTENSION_API_V1,
};
use types::{DiskProbe, HomeboyProbe, MemoryProbe, RunnerCapabilities, RunnerCheck, ToolProbe};

pub(crate) fn tool_specs(runner: &Runner) -> Vec<RunnerToolSpec> {
    RunnerToolRegistry::doctor_tools(runner)
}

pub(crate) fn capabilities_from(
    local_execution: bool,
    ssh_execution: bool,
    homeboy_available: bool,
    workspace_writable: bool,
    artifact_store_available: bool,
) -> RunnerCapabilities {
    RunnerCapabilities {
        local_execution,
        ssh_execution,
        homeboy_available,
        workspace_writable,
        artifact_store_available,
    }
}

pub(crate) fn declared_tool_specs_by_source() -> BTreeMap<String, Vec<RunnerToolSpec>> {
    declared_extension_tool_specs_by_source()
}

pub(super) fn declared_extension_tool_specs_by_source() -> BTreeMap<String, Vec<RunnerToolSpec>> {
    let catalog = homeboy_core::extension::catalog::list_api(&ExtensionApiCatalogRequest {
        schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
    });
    let mut by_source = BTreeMap::new();
    for entry in catalog.entries {
        let Some(descriptor) = (entry.status == ExtensionApiCatalogEntryStatus::Available)
            .then_some(entry.descriptor)
            .flatten()
        else {
            continue;
        };
        let mut specs = descriptor
            .execution_requirements
            .tools
            .iter()
            .filter_map(|tool| {
                declared_tool_spec_from_parts(
                    &descriptor.identity.id,
                    &tool.id,
                    Some(tool.command.as_str()),
                    &tool.version_args,
                    tool.remediation.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        if specs.is_empty() {
            continue;
        }
        specs.sort_by(|left, right| left.id.cmp(&right.id));
        by_source.insert(descriptor.identity.id, specs);
    }
    by_source
}

fn declared_tool_spec_from_parts(
    source: &str,
    tool: &str,
    command: Option<&str>,
    version_args: &[String],
    remediation: Option<&str>,
) -> Option<RunnerToolSpec> {
    let id = tool.trim();
    if id.is_empty() {
        return None;
    }
    Some(RunnerToolSpec {
        tool: None,
        id: id.to_string(),
        capability_id: format!("declared_tool.{source}.{id}"),
        check_id: format!("tool.declared.{source}.{id}"),
        command: command.unwrap_or(id).to_string(),
        version_args: if version_args.is_empty() {
            vec!["--version".to_string()]
        } else {
            version_args.to_vec()
        },
        required: false,
        remediation: remediation
            .map(str::to_string)
            .unwrap_or_else(|| format!("Install '{id}' and ensure it is on PATH")),
        capability_remediation: remediation
            .map(str::to_string)
            .unwrap_or_else(|| format!("Install '{id}' on the runner and ensure it is on PATH.")),
    })
}

pub(crate) fn tool_available(tools: &BTreeMap<String, ToolProbe>, id: &str) -> bool {
    tools.get(id).map(|tool| tool.available).unwrap_or(false)
}

pub(crate) fn required_tool_version_args(command: &str) -> Vec<String> {
    let name = Path::new(command)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(command);
    if name == "homeboy" {
        vec!["--version".to_string()]
    } else {
        Vec::new()
    }
}

pub(crate) fn local_tool_probe(command: &str, version_args: &[String]) -> ToolProbe {
    let path = match common::resolve_on_path(command) {
        common::PathLookup::Found(path) => path,
        common::PathLookup::NotFound => return ToolProbe::not_found(),
        common::PathLookup::Unavailable(reason) => {
            return ToolProbe::probe_failed(format!("tool lookup could not run: {reason}"))
        }
    };
    let version = if version_args.is_empty() {
        None
    } else {
        Command::new(command)
            .args(version_args)
            .output()
            .ok()
            .and_then(|output| {
                if output.status.success() {
                    common::first_output_line(&output.stdout, &output.stderr)
                } else {
                    None
                }
            })
    };
    ToolProbe::found(path, version)
}

pub(crate) fn remote_tool_probe(
    client: &SshClient,
    command: &str,
    version_args: &[String],
) -> ToolProbe {
    let path = match common::remote_path_lookup(client, command) {
        common::PathLookup::Found(path) => path,
        common::PathLookup::NotFound => return ToolProbe::not_found(),
        common::PathLookup::Unavailable(stderr) => {
            return ToolProbe::probe_failed(format!("tool probe shell failed: {stderr}"))
        }
    };
    let version = if version_args.is_empty() {
        None
    } else {
        let args = version_args
            .iter()
            .map(|arg| common::shell_word(arg))
            .collect::<Vec<_>>()
            .join(" ");
        common::remote_line(
            client,
            &format!(
                "{} {} 2>&1 | sed -n '1p'",
                common::shell_word(command),
                args
            ),
        )
    };
    ToolProbe::found(path, version)
}

pub(crate) fn lab_homeboy_path_checks(
    client: &SshClient,
    runner_id: &str,
    server_id: &str,
    configured_command: &str,
    local_version: &str,
    configured_probe: &HomeboyProbe,
) -> Vec<RunnerCheck> {
    let mut checks = Vec::new();
    let bare = remote_homeboy_probe(client, "homeboy");
    let candidate = remote_preferred_homeboy_candidate(client, local_version);

    let mut configured_details = BTreeMap::new();
    configured_details.insert(
        "configured_command".to_string(),
        configured_command.to_string(),
    );
    configured_details.insert(
        "configured_version".to_string(),
        configured_probe.version.clone(),
    );
    if let Some(path) = &configured_probe.path {
        configured_details.insert("configured_path".to_string(), path.clone());
    }
    if let Some(path) = &bare.path {
        configured_details.insert("bare_path".to_string(), path.clone());
    }
    if let Some(version) = &bare.version {
        configured_details.insert("bare_version".to_string(), version.clone());
    }
    if let Some(candidate) = &candidate {
        configured_details.insert("preferred_path".to_string(), candidate.path.clone());
        configured_details.insert("preferred_version".to_string(), candidate.version.clone());
    }

    checks.push(checks::ok_with_details(
        "lab.homeboy.configured",
        format!(
            "Lab runner configured Homeboy command reports {}",
            configured_probe.version
        ),
        configured_details.clone(),
    ));

    if let Some(candidate) = candidate {
        if let Some(check) = lab_homeboy_path_drift_check(
            runner_id,
            configured_probe,
            &candidate,
            configured_details.clone(),
        ) {
            checks.push(check);
        }
    }

    if let Some(check) = homeboy_path_shadow_check(
        runner_id,
        server_id,
        configured_command,
        local_version,
        configured_probe,
        &bare,
        configured_details,
    ) {
        checks.push(check);
    }

    checks
}

pub(super) fn lab_homeboy_path_drift_check(
    runner_id: &str,
    configured: &HomeboyProbe,
    preferred: &RemoteHomeboyCandidate,
    details: BTreeMap<String, String>,
) -> Option<RunnerCheck> {
    let configured_version = configured.version.trim();
    let preferred_version = preferred.version.trim();
    if configured_version == preferred_version {
        return None;
    }

    let preferred_is_dirty = preferred_version.contains("dirty");
    let preferred_precedence = (!preferred_is_dirty)
        .then(|| {
            let configured =
                semver::Version::parse(configured_version.trim_start_matches('v')).ok()?;
            let preferred =
                semver::Version::parse(preferred_version.trim_start_matches('v')).ok()?;
            Some(preferred.cmp_precedence(&configured))
        })
        .flatten();

    if preferred_precedence.is_some_and(|precedence| precedence.is_gt()) {
        return Some(checks::warning_with_details(
            "lab.homeboy.path_drift",
            format!(
                "Preferred runner Homeboy {preferred_version} at {} is newer than configured runner Homeboy {configured_version}",
                preferred.path
            ),
            Some(format!(
                "Point runner `{runner_id}` at the newer preferred binary with `homeboy runner set {runner_id} --json '{{\"homeboy_path\":\"{}\"}}'`",
                preferred.path
            )),
            details,
        ));
    }

    let reason = if preferred_is_dirty {
        "is a dirty build"
    } else if preferred_precedence.is_some_and(|precedence| precedence.is_lt()) {
        "is older than configured"
    } else {
        "has a different build identity from configured"
    };
    Some(checks::warning_with_details(
        "lab.homeboy.path_drift",
        format!(
            "Preferred runner Homeboy {preferred_version} at {} {reason}; configured Homeboy {configured_version} remains selected",
            preferred.path
        ),
        Some(format!(
            "Update or remove stale preferred binary at {}; keep runner `{runner_id}` on configured homeboy_path `{}`",
            preferred.path,
            configured.path.as_deref().unwrap_or("homeboy")
        )),
        details,
    ))
}

pub(super) fn homeboy_path_shadow_check(
    runner_id: &str,
    server_id: &str,
    configured_command: &str,
    local_version: &str,
    configured_probe: &HomeboyProbe,
    bare: &RemoteHomeboyCandidateProbe,
    details: BTreeMap<String, String>,
) -> Option<RunnerCheck> {
    let bare_version = bare.version.as_deref()?.trim();
    if bare_version.is_empty() {
        return None;
    }

    if configured_command == "homeboy" {
        let local_version = local_version.trim();
        if bare_version == local_version {
            return None;
        }
        return Some(checks::warning_with_details(
            "lab.homeboy.path_shadow",
            format!(
                "Runner PATH resolves bare `homeboy` to {bare_version}, but local Homeboy is {local_version}"
            ),
            Some(format!(
                "Configure runner `{runner_id}` with an absolute current homeboy_path, or fix PATH ordering on server `{server_id}`"
            )),
            details,
        ));
    }

    let configured_path = configured_probe
        .path
        .as_deref()
        .unwrap_or(configured_command);
    let bare_path = bare.path.as_deref().unwrap_or("homeboy");
    let configured_version = configured_probe.version.trim();
    if configured_version.is_empty()
        || configured_version == "unknown"
        || !version_is_older(bare_version, configured_version)
    {
        if configured_command != "homeboy" && configured_path != bare_path {
            return Some(checks::ok_with_details(
                "lab.homeboy.path_shadow",
                format!(
                    "Runner uses configured Homeboy at {configured_path}; bare PATH `homeboy` resolves to {bare_path}"
                ),
                details,
            ));
        }

        return None;
    }

    Some(checks::ok(
        "lab.homeboy.path_shadow",
        format!(
            "Runner uses configured Homeboy {configured_version} at {configured_path}; bare PATH `homeboy` is older ({bare_version} at {bare_path})"
        ),
        Some(format!(
            "No runner repair is needed while jobs use configured homeboy_path `{configured_command}`. To clean up interactive PATH skew, run `homeboy ssh {server_id} -- 'command -v homeboy && homeboy --version'` and update/remove the stale bare `homeboy`."
        )),
    ))
    .map(|mut check| {
        check.details = details;
        check
    })
}

fn version_is_older(candidate: &str, baseline: &str) -> bool {
    let candidate = semantic_version_parts(candidate);
    let baseline = semantic_version_parts(baseline);
    !candidate.is_empty() && !baseline.is_empty() && candidate < baseline
}

fn semantic_version_parts(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>()
        })
        .take_while(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
}

pub(crate) fn provider_readiness_checks(
    client: &SshClient,
    contracts: &[homeboy::agents::agent_tasks::provider::AgentTaskProviderRunnerReadinessContract],
) -> Vec<RunnerCheck> {
    contracts
        .iter()
        .filter_map(|contract| {
            provider_readiness_check(client, contract).map(|mut check| {
                check
                    .details
                    .entry("provider_id".to_string())
                    .or_insert_with(|| contract.provider_id.clone());
                check
            })
        })
        .collect()
}

pub(crate) fn selected_provider_readiness_contracts(
    contracts: Vec<
        homeboy::agents::agent_tasks::provider::AgentTaskProviderRunnerReadinessContract,
    >,
    provider_ids: &[String],
) -> Vec<homeboy::agents::agent_tasks::provider::AgentTaskProviderRunnerReadinessContract> {
    contracts
        .into_iter()
        .filter(|contract| provider_ids.contains(&contract.provider_id))
        .collect()
}

pub(crate) fn eligible_provider_ids(
    providers: &[AgentTaskExecutorProvider],
    selected_backend: Option<&str>,
    selected_provider_id: Option<&str>,
) -> Vec<String> {
    selected_providers(providers, selected_backend, selected_provider_id)
        .into_iter()
        .map(|provider| provider.id.clone())
        .collect()
}

fn selected_providers<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    selected_backend: Option<&str>,
    selected_provider_id: Option<&str>,
) -> Vec<&'a AgentTaskExecutorProvider> {
    providers
        .iter()
        .filter(|provider| {
            selected_backend.is_none_or(|backend| provider.backend == backend)
                && selected_provider_id.is_none_or(|provider_id| provider.id == provider_id)
        })
        .collect()
}

/// #3818: report the state of every extension-declared managed runner
/// source checkout. Surfaces a missing checkout (error) or a checkout that
/// tracks a different remote than the declared canonical remote (warning)
/// so operators see drift before a cook runs against a stale source.
pub(crate) fn managed_runner_source_checks(
    client: &SshClient,
    contracts: &[AgentTaskProviderRunnerSource],
) -> Vec<RunnerCheck> {
    contracts
        .iter()
        .map(|contract| managed_runner_source_check(client, contract))
        .collect()
}

fn managed_runner_source_check(
    client: &SshClient,
    contract: &AgentTaskProviderRunnerSource,
) -> RunnerCheck {
    let id = format!("lab.managed_source.{}", contract.id);
    let mut details = BTreeMap::new();
    // Resolve the declared path through the runner shell so `~`/`$HOME`
    // expand to the runner user's real home.
    let resolved_path = common::remote_line(
        client,
        &format!("printf '%s\n' {}", common::shell_path_expr(&contract.path)),
    )
    .filter(|value| !value.trim().is_empty())
    .unwrap_or_else(|| contract.path.clone());
    details.insert("path".to_string(), resolved_path.clone());
    if let Some(remote_url) = contract.remote_url.as_deref() {
        details.insert("declared_remote".to_string(), remote_url.to_string());
    }
    if let Some(git_ref) = contract.git_ref.as_deref() {
        details.insert("declared_ref".to_string(), git_ref.to_string());
    }

    let is_git = client
        .execute(&format!(
            "test -d {}/.git",
            common::shell_word(&resolved_path)
        ))
        .success;
    if !is_git {
        // `repair::repair_managed_sources` runs the declared sync script and
        // then re-probes, so a missing checkout is definitively within its
        // reach. The remote-mismatch case below is deliberately left without an
        // action: whether a sync re-points an existing remote depends on the
        // provider's script, and an action that cannot fix its check reports as
        // ineffective at best and repairs the wrong thing at worst.
        return checks::error(
            id,
            format!(
                "Managed runner source `{}` is not present as a git checkout on the Lab runner",
                contract.label
            ),
            contract.remediation.clone(),
            details,
        )
        .with_action(types::RunnerRepairAction::RefreshManagedSources);
    }

    let actual_remote = common::remote_line(
        client,
        &format!(
            "git -C {} config --get remote.origin.url 2>/dev/null",
            common::shell_word(&resolved_path)
        ),
    );
    if let Some(actual_remote) = actual_remote.as_deref() {
        details.insert("origin_remote".to_string(), actual_remote.to_string());
    }
    if let Some(head) = common::remote_line(
        client,
        &format!(
            "git -C {} rev-parse --short HEAD 2>/dev/null",
            common::shell_word(&resolved_path)
        ),
    ) {
        details.insert("head".to_string(), head);
    }

    let branch = common::remote_line(
        client,
        &format!(
            "git -C {} symbolic-ref --quiet --short HEAD 2>/dev/null",
            common::shell_word(&resolved_path)
        ),
    );
    if let Some(branch) = branch.as_deref() {
        details.insert("branch".to_string(), branch.to_string());
    }

    let dirty_files = common::remote_line(
        client,
        &format!(
            "git -C {} status --porcelain 2>/dev/null | wc -l | tr -d ' '",
            common::shell_word(&resolved_path)
        ),
    )
    .and_then(|value| value.parse::<u64>().ok())
    .unwrap_or(0);
    details.insert("dirty_files".to_string(), dirty_files.to_string());

    if let (Some(declared_remote), Some(actual_remote)) =
        (contract.remote_url.as_deref(), actual_remote.as_deref())
    {
        if declared_remote != actual_remote {
            return checks::warning_with_details(
                id,
                format!(
                    "Managed runner source `{}` tracks a different remote than declared on the Lab runner",
                    contract.label
                ),
                contract.remediation.clone(),
                details,
            );
        }
    }

    if let Some(check) = managed_runner_source_state_check(
        contract,
        id.clone(),
        branch.as_deref(),
        dirty_files,
        details.clone(),
    ) {
        return check;
    }

    checks::ok_with_details(
        id,
        format!(
            "Managed runner source `{}` is present on the Lab runner",
            contract.label
        ),
        details,
    )
}

pub(super) fn managed_runner_source_state_check(
    contract: &AgentTaskProviderRunnerSource,
    id: String,
    branch: Option<&str>,
    dirty_files: u64,
    details: BTreeMap<String, String>,
) -> Option<RunnerCheck> {
    if dirty_files > 0 {
        return Some(checks::warning_with_details(
            id,
            format!(
                "Managed runner source `{}` has reconstructable local modifications on the Lab runner",
                contract.label
            ),
            contract.remediation.clone(),
            details,
        ));
    }

    let git_ref = contract
        .git_ref
        .as_deref()
        .filter(|value| !value.trim().is_empty())?;
    let branch = branch.unwrap_or("").trim();
    if branch != git_ref {
        return Some(checks::warning_with_details(
            id,
            format!(
                "Managed runner source `{}` is not on declared ref `{git_ref}` on the Lab runner",
                contract.label
            ),
            contract.remediation.clone(),
            details,
        ));
    }

    None
}

fn provider_readiness_check(
    client: &SshClient,
    contract: &homeboy::agents::agent_tasks::provider::AgentTaskProviderRunnerReadinessContract,
) -> Option<RunnerCheck> {
    let contract = &contract.readiness;
    let env_path = contract.env_path.as_ref()?;
    let env_names = env_path
        .env
        .iter()
        .map(|name| common::shell_word(name))
        .collect::<Vec<_>>()
        .join(" ");
    let path = common::remote_line(
        client,
        &format!(
            "for name in {env_names}; do candidate=$(printenv \"$name\" 2>/dev/null || true); [ -n \"$candidate\" ] && printf '%s\n' \"$candidate\" && exit 0; done; exit 1"
        ),
    )
    .filter(|value| !value.trim().is_empty());
    let Some(path) = path else {
        return Some(provider_env_path_readiness_check_from_probe(
            contract, None, false, None, None,
        ));
    };

    let mut details = BTreeMap::new();
    details.insert("path".to_string(), path.clone());
    details.insert("env".to_string(), env_path.env.join(","));
    let exists = client
        .execute(&format!("test -e {}", common::shell_word(&path)))
        .success;
    if !exists {
        return Some(provider_env_path_readiness_check_from_probe(
            contract,
            Some(path),
            false,
            None,
            None,
        ));
    }

    if env_path.revision.unwrap_or(false) {
        if let Some(revision) = common::remote_line(
            client,
            &format!(
                "p={}; if [ -d \"$p/.git\" ]; then git -C \"$p\" rev-parse --short HEAD 2>/dev/null; elif [ -d \"$(dirname \"$p\")/.git\" ]; then git -C \"$(dirname \"$p\")\" rev-parse --short HEAD 2>/dev/null; fi",
                common::shell_word(&path)
            ),
        ) {
            details.insert("revision".to_string(), revision);
        }
    }

    // #4140: resolve the extension-declared canonical root on the runner
    // (expanding `~`/`$HOME` etc. via the runner shell) so we can warn when
    // the env-resolved tool path lives outside the managed checkout.
    let resolved_canonical = env_path
        .canonical_path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .and_then(|canonical| {
            common::remote_line(
                client,
                &format!("printf '%s\n' {}", common::shell_word(canonical)),
            )
            .filter(|value| !value.trim().is_empty())
        });

    Some(provider_env_path_readiness_check_from_probe(
        contract,
        Some(path),
        true,
        details.get("revision").cloned(),
        resolved_canonical,
    ))
}

pub(super) fn path_within_canonical_root(path: &str, canonical_root: &str) -> bool {
    let normalize = |value: &str| {
        let trimmed = value.trim().trim_end_matches('/');
        trimmed.to_string()
    };
    let path = normalize(path);
    let root = normalize(canonical_root);
    if root.is_empty() {
        return true;
    }
    if path == root {
        return true;
    }
    path.starts_with(&format!("{root}/"))
}

pub(super) fn provider_env_path_readiness_check_from_probe(
    contract: &AgentTaskProviderRunnerReadiness,
    path: Option<String>,
    exists: bool,
    revision: Option<String>,
    canonical_path: Option<String>,
) -> RunnerCheck {
    let env = contract
        .env_path
        .as_ref()
        .map(|env_path| env_path.env.join(","))
        .unwrap_or_default();
    let mut details = BTreeMap::new();
    if !env.is_empty() {
        details.insert("env".to_string(), env);
    }
    let resolved_path = path.clone();
    if let Some(path) = path {
        details.insert("path".to_string(), path);
    }
    if let Some(revision) = revision {
        details.insert("revision".to_string(), revision);
    }
    if let Some(canonical_path) = canonical_path.as_deref() {
        details.insert("canonical_path".to_string(), canonical_path.to_string());
    }

    if !details.contains_key("path") {
        return checks::warning_with_details(
            contract.id.clone(),
            format!(
                "{} path is not configured in the Lab runner environment",
                contract.label
            ),
            contract.remediation.clone(),
            details,
        );
    }

    if !exists {
        return checks::error(
            contract.id.clone(),
            format!(
                "Configured {} path does not exist on the Lab runner",
                contract.label
            ),
            contract.remediation.clone(),
            details,
        );
    }

    // #4140: the path resolves to a real checkout, but if the declaring
    // extension pinned a canonical managed root and the resolved path lives
    // outside it, the runner is using a stale / non-canonical checkout that
    // can corrupt results. Surface this as a warning before it does.
    if let (Some(resolved_path), Some(canonical_root)) =
        (resolved_path.as_deref(), canonical_path.as_deref())
    {
        if !path_within_canonical_root(resolved_path, canonical_root) {
            return checks::warning_with_details(
                contract.id.clone(),
                format!(
                    "{} resolves to a non-canonical checkout outside the managed source root on the Lab runner",
                    contract.label
                ),
                contract.remediation.clone(),
                details,
            );
        }
    }

    checks::ok_with_details(
        contract.id.clone(),
        format!("{} path exists on the Lab runner", contract.label),
        details,
    )
}

pub(super) struct RemoteHomeboyCandidate {
    pub(super) path: String,
    pub(super) version: String,
}

fn remote_homeboy_probe(client: &SshClient, command: &str) -> RemoteHomeboyCandidateProbe {
    RemoteHomeboyCandidateProbe {
        path: common::remote_line(
            client,
            &format!("command -v {}", common::shell_word(command)),
        ),
        version: remote_homeboy_version(client, command),
    }
}

pub(super) struct RemoteHomeboyCandidateProbe {
    pub(super) path: Option<String>,
    pub(super) version: Option<String>,
}

fn remote_preferred_homeboy_candidate(
    client: &SshClient,
    local_version: &str,
) -> Option<RemoteHomeboyCandidate> {
    let command = "for p in \"$HOME/.cargo/bin/homeboy\" \"$HOME/.local/bin/homeboy\" /usr/local/bin/homeboy; do [ -x \"$p\" ] || continue; v=$(\"$p\" --version 2>/dev/null | awk '{print $2}'); [ -n \"$v\" ] || continue; printf '%s %s\n' \"$p\" \"$v\"; done";
    let output = client.execute(command);
    if !output.success {
        return None;
    }
    let mut first = None;
    for line in output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let mut parts = line.split_whitespace();
        let path = parts.next()?.to_string();
        let version = parts.next()?.to_string();
        let candidate = RemoteHomeboyCandidate { path, version };
        if candidate.version.trim() == local_version.trim() {
            return Some(candidate);
        }
        first.get_or_insert(candidate);
    }
    first
}

fn remote_homeboy_version(client: &SshClient, command: &str) -> Option<String> {
    common::remote_line(
        client,
        &format!(
            "{} --version 2>/dev/null | awk '{{print $2}}'",
            common::shell_word(command)
        ),
    )
}

pub(crate) fn local_memory_probe() -> Option<MemoryProbe> {
    memory_from_proc_meminfo().or_else(memory_from_macos_sysctl)
}

pub(crate) fn remote_memory_probe(client: &SshClient) -> Option<MemoryProbe> {
    let total_mb = common::remote_line(
        client,
        "awk '/MemTotal:/ {print int($2/1024)}' /proc/meminfo 2>/dev/null || expr $(sysctl -n hw.memsize 2>/dev/null) / 1048576",
    )?
    .parse::<u64>()
    .ok()?;
    let available_mb = common::remote_line(
        client,
        "awk '/MemAvailable:/ {print int($2/1024)}' /proc/meminfo 2>/dev/null",
    )
    .and_then(|value| value.parse::<u64>().ok());
    Some(MemoryProbe {
        total_mb,
        available_mb,
    })
}

#[cfg(unix)]
pub(crate) fn local_disk_probe(path: &Path) -> Option<DiskProbe> {
    let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    let block_size = u128::from(stat.f_frsize.max(1));
    let total_blocks = u128::from(stat.f_blocks);
    let available_blocks = u128::from(stat.f_bavail);
    Some(DiskProbe {
        path: common::display_path(path),
        total_mb: (total_blocks.saturating_mul(block_size) / 1024 / 1024)
            .try_into()
            .ok()?,
        available_mb: (available_blocks.saturating_mul(block_size) / 1024 / 1024)
            .try_into()
            .ok()?,
    })
}

#[cfg(not(unix))]
pub(crate) fn local_disk_probe(_path: &Path) -> Option<DiskProbe> {
    None
}

pub(crate) fn remote_disk_probe(client: &SshClient, path: &str) -> Option<DiskProbe> {
    let line = common::remote_line(
        client,
        &format!(
            "df -Pk {} | awk 'NR==2 {{print $2 \" \" $4}}'",
            common::shell_word(path)
        ),
    )?;
    let mut parts = line.split_whitespace();
    let total_kb = parts.next()?.parse::<u64>().ok()?;
    let available_kb = parts.next()?.parse::<u64>().ok()?;
    Some(DiskProbe {
        path: path.to_string(),
        total_mb: total_kb / 1024,
        available_mb: available_kb / 1024,
    })
}

pub(crate) fn local_browser_ready() -> bool {
    browser_cache_candidates().into_iter().any(|path| {
        path.is_dir()
            && fs::read_dir(path)
                .map(|mut entries| entries.next().is_some())
                .unwrap_or(false)
    })
}

pub(crate) fn remote_browser_ready(client: &SshClient) -> bool {
    let command = "for d in \"${PLAYWRIGHT_BROWSERS_PATH:-}\" \"$HOME/Library/Caches/ms-playwright\" \"$HOME/.cache/ms-playwright\"; do [ -n \"$d\" ] && [ -d \"$d\" ] && find \"$d\" -mindepth 1 -maxdepth 1 2>/dev/null | grep -q . && exit 0; done; exit 1";
    client.execute(command).success
}

pub(crate) fn headed_browser_ready(display_ready: bool, xvfb_ready: bool) -> bool {
    display_ready || xvfb_ready
}

pub(crate) fn local_display_ready() -> bool {
    env::var("DISPLAY").is_ok_and(|value| !value.trim().is_empty())
}

pub(crate) fn remote_display_ready(client: &SshClient) -> bool {
    client.execute("[ -n \"${DISPLAY:-}\" ]").success
}

pub(crate) fn local_xvfb_ready() -> bool {
    local_tool_probe("xvfb-run", &[]).available || local_tool_probe("Xvfb", &[]).available
}

pub(crate) fn remote_xvfb_ready(client: &SshClient) -> bool {
    remote_tool_probe(client, "xvfb-run", &[]).available
        || remote_tool_probe(client, "Xvfb", &[]).available
}

#[cfg(unix)]
pub(crate) fn local_path_writable(path: &Path) -> bool {
    let c_path = match std::ffi::CString::new(path.to_string_lossy().as_bytes()) {
        Ok(path) => path,
        Err(_) => return false,
    };
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(not(unix))]
pub(crate) fn local_path_writable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| !metadata.permissions().readonly())
        .unwrap_or(false)
}

pub(crate) fn local_path_or_parent_writable(path: &Path) -> bool {
    if path.exists() {
        local_path_writable(path)
    } else {
        path.parent().map(local_path_writable).unwrap_or(false)
    }
}

pub(crate) fn remote_path_writable(client: &SshClient, path: &str) -> bool {
    client.execute(&remote_path_writable_command(path)).success
}

pub(super) fn remote_path_writable_command(path: &str) -> String {
    format!("test -w {}", common::shell_path_expr(path))
}

pub(crate) fn remote_artifact_store_available(client: &SshClient, path: &str) -> bool {
    client
        .execute(&remote_artifact_store_available_command(path))
        .success
}

pub(super) fn remote_artifact_store_available_command(path: &str) -> String {
    let path = common::shell_path_expr(path);
    format!(
        "if [ -e {path} ]; then test -d {path} && test -w {path}; else parent=$(dirname {path}); test -d \"$parent\" && test -w \"$parent\"; fi"
    )
}

pub(crate) fn connected_daemon_exec_checks(
    runner_id: &str,
    workspace_root: &str,
) -> Vec<RunnerCheck> {
    connected_daemon_exec_checks_with_timeout(
        runner_id,
        workspace_root,
        std::time::Duration::from_secs(5),
    )
}

pub(crate) fn connected_daemon_exec_checks_with_timeout(
    runner_id: &str,
    workspace_root: &str,
    timeout: std::time::Duration,
) -> Vec<RunnerCheck> {
    let Ok(status) = runner::status(runner_id) else {
        return Vec::new();
    };
    if !status.connected {
        return Vec::new();
    }
    let Some(session) = status.session else {
        return Vec::new();
    };
    let Some(local_url) = session.local_url else {
        return vec![checks::error(
            "daemon.exec",
            "Connected direct runner session is missing its local daemon URL".to_string(),
            Some(format!(
                "Reconnect runner `{runner_id}` with `homeboy runner connect {runner_id}`"
            )),
            BTreeMap::new(),
        )];
    };

    vec![daemon_exec_check_with_timeout(
        runner_id,
        workspace_root,
        &local_url,
        timeout,
    )]
}

/// Observe the already-published daemon session without creating a daemon job
/// or invoking runner status reconciliation. This is the same liveness
/// predicate status and `ensure_transport` use: the recorded tunnel reaches a
/// daemon whose `/health` proves the session's lease and PID. It never opens a
/// tunnel or rewrites the session.
pub(crate) fn connected_daemon_health_checks_with_timeout(
    runner_id: &str,
    session: &RunnerSession,
    timeout: std::time::Duration,
) -> Vec<RunnerCheck> {
    let Some(local_url) = session.local_url.as_deref() else {
        return Vec::new();
    };
    let mut details = BTreeMap::new();
    details.insert("url".to_string(), local_url.to_string());
    if timeout.is_zero() {
        details.insert(
            "reason_code".to_string(),
            "runner_doctor.overall_timeout".to_string(),
        );
        return vec![checks::error(
            "daemon.exec",
            "Connected runner daemon health probe was skipped because the diagnostic deadline was exhausted".to_string(),
            None,
            details,
        )];
    }
    if runner::transport_is_live(session, timeout) {
        return vec![checks::ok(
            "daemon.exec",
            "Connected runner daemon health and session identity are valid".to_string(),
            None,
        )];
    }
    details.insert(
        "expected_lease".to_string(),
        session
            .remote_daemon_lease_id
            .clone()
            .unwrap_or_else(|| "unavailable".to_string()),
    );
    details.insert(
        "expected_pid".to_string(),
        session
            .remote_daemon_pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "unavailable".to_string()),
    );
    details.insert(
        "reason_code".to_string(),
        "runner_doctor.daemon_liveness_unproven".to_string(),
    );
    vec![checks::error(
        "daemon.exec",
        "Runner daemon /health does not prove the controller session's lease and PID".to_string(),
        Some(format!(
            "Re-establish the transport with `homeboy runner doctor {runner_id} --scope lab-offload --repair` after inspecting `homeboy runner status {runner_id}`"
        )),
        details,
    )]
}

pub(super) fn daemon_exec_check_with_timeout(
    runner_id: &str,
    workspace_root: &str,
    local_url: &str,
    timeout: std::time::Duration,
) -> RunnerCheck {
    let mut details = BTreeMap::new();
    details.insert("url".to_string(), local_url.to_string());
    details.insert("cwd".to_string(), workspace_root.to_string());
    if timeout.is_zero() {
        details.insert(
            "reason_code".to_string(),
            "runner_doctor.overall_timeout".to_string(),
        );
        details.insert("timeout_ms".to_string(), "0".to_string());
        return checks::error(
            "daemon.exec",
            "Connected runner daemon probe was skipped because the overall diagnostic deadline was exhausted".to_string(),
            Some(format!(
                "Rerun `homeboy runner doctor {runner_id} --scope lab-offload` after reconnecting the runner if needed"
            )),
            details,
        );
    }
    let client = match reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            details.insert("error".to_string(), err.to_string());
            let reason_code =
                if err.is_timeout() || err.to_string().to_ascii_lowercase().contains("timed out") {
                    "runner_doctor.daemon_timeout"
                } else {
                    "runner_doctor.daemon_request_failed"
                };
            details.insert("reason_code".to_string(), reason_code.to_string());
            details.insert("timeout_ms".to_string(), timeout.as_millis().to_string());
            return checks::error(
                "daemon.exec",
                "Could not build daemon exec probe HTTP client".to_string(),
                None,
                details,
            );
        }
    };
    let probe_request = homeboy_core::api_jobs::RemoteRunnerJobRequest {
        runner_id: runner_id.to_string(),
        project_id: None,
        operation: "runner.exec".to_string(),
        command: vec!["homeboy".to_string(), "--version".to_string()],
        cwd: Some(workspace_root.to_string()),
        env: Default::default(),
        secret_env_names: Vec::new(),
        secret_env_plan: Default::default(),
        env_materialization: None,
        capture_patch: false,
        source_snapshot: None,
        path_materialization_plan: None,
        require_paths: Vec::new(),
        extension_env_providers: Vec::new(),
        lab_runner_workload: None,
        lifecycle: None,
        workspace_claim_binding: None,
        workspace_owner_lease: None,
        metadata: None,
    };
    let submission = homeboy_core::daemon::DirectDaemonExecSubmitRequest::new(
        &probe_request,
        format!("runner-doctor:v1:{runner_id}:{}", uuid::Uuid::new_v4()),
    );
    let response = client
        .post(format!("{}/exec", local_url.trim_end_matches('/')))
        .json(&submission)
        .send();
    let response = match response {
        Ok(response) => response,
        Err(err) => {
            details.insert("error".to_string(), err.to_string());
            let reason_code =
                if err.is_timeout() || err.to_string().to_ascii_lowercase().contains("timed out") {
                    "runner_doctor.daemon_timeout"
                } else {
                    "runner_doctor.daemon_request_failed"
                };
            details.insert("reason_code".to_string(), reason_code.to_string());
            details.insert("timeout_ms".to_string(), timeout.as_millis().to_string());
            return checks::error(
                "daemon.exec",
                "Connected runner daemon did not accept the exec probe".to_string(),
                Some(format!(
                    "Reconnect runner `{runner_id}` with `homeboy runner connect {runner_id}` before retrying Lab offload"
                )),
                details,
            );
        }
    };
    let status_code = response.status().as_u16();
    let body: serde_json::Value = match response.json() {
        Ok(body) => body,
        Err(err) => {
            details.insert("status".to_string(), status_code.to_string());
            details.insert("error".to_string(), err.to_string());
            details.insert(
                "reason_code".to_string(),
                "runner_doctor.daemon_response_failed".to_string(),
            );
            details.insert("timeout_ms".to_string(), timeout.as_millis().to_string());
            return checks::error(
                "daemon.exec",
                "Connected runner daemon returned an invalid exec probe response".to_string(),
                Some(format!(
                    "Reconnect runner `{runner_id}` with `homeboy runner connect {runner_id}` before retrying Lab offload"
                )),
                details,
            );
        }
    };
    details.insert("status".to_string(), status_code.to_string());
    if let Some(job_id) = body
        .pointer("/data/body/job/id")
        .and_then(serde_json::Value::as_str)
    {
        details.insert("job_id".to_string(), job_id.to_string());
    }
    if status_code < 400 && body.get("success").and_then(serde_json::Value::as_bool) == Some(true) {
        return checks::ok_with_details(
            "daemon.exec",
            "Connected runner daemon accepted a lightweight exec probe".to_string(),
            details,
        );
    }

    let error_payload = body
        .get("error")
        .or_else(|| body.get("data"))
        .unwrap_or(&body);
    details.insert("response".to_string(), error_payload.to_string());
    checks::error(
        "daemon.exec",
        "Connected runner daemon failed the lightweight exec probe".to_string(),
        Some(format!(
            "Reconnect runner `{runner_id}` with `homeboy runner connect {runner_id}` before retrying Lab offload"
        )),
        details,
    )
}

fn memory_from_proc_meminfo() -> Option<MemoryProbe> {
    let raw = fs::read_to_string("/proc/meminfo").ok()?;
    let total_kb = meminfo_value_kb(&raw, "MemTotal")?;
    let available_kb = meminfo_value_kb(&raw, "MemAvailable");
    Some(MemoryProbe {
        total_mb: total_kb / 1024,
        available_mb: available_kb.map(|kb| kb / 1024),
    })
}

fn memory_from_macos_sysctl() -> Option<MemoryProbe> {
    let total_bytes = common::local_command_line("sysctl", &["-n", "hw.memsize"])?;
    let total_mb = total_bytes.parse::<u64>().ok()? / 1024 / 1024;
    Some(MemoryProbe {
        total_mb,
        available_mb: None,
    })
}

fn meminfo_value_kb(raw: &str, key: &str) -> Option<u64> {
    raw.lines().find_map(|line| {
        let (name, rest) = line.split_once(':')?;
        if name != key {
            return None;
        }
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

fn browser_cache_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = env::var("PLAYWRIGHT_BROWSERS_PATH") {
        if !path.trim().is_empty() {
            candidates.push(PathBuf::from(path));
        }
    }
    if let Ok(home) = homeboy::core::paths::home_root() {
        candidates.push(home.join("Library").join("Caches").join("ms-playwright"));
        candidates.push(home.join(".cache").join("ms-playwright"));
    }
    candidates
}
