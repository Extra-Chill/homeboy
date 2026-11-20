use super::*;

/// Extensions declared by the admitted component and by every portable
/// `homeboy.json` in the executing checkout (#15383, #17569).
pub(super) fn declared_component_extension_ids(
    admitted_component_id: Option<&str>,
    cwd: &Path,
) -> std::collections::BTreeSet<String> {
    let mut components = Vec::new();
    if let Ok(component) = homeboy_core::component::resolve_effective(
        admitted_component_id,
        Some(&cwd.to_string_lossy()),
        None,
    ) {
        components.push(component);
    }
    // Resolve portable sibling contracts from the executing checkout, not
    // controller registry paths. Git excludes ignored dependency trees.
    if let Some((root, files)) = homeboy_core::git::repo_root(cwd).and_then(|root| {
        homeboy_core::git::output_optional(
            &root,
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ],
        )
        .map(|files| (root, files))
    }) {
        for file in files.split('\0') {
            let path = Path::new(file);
            if !path.file_name().is_some_and(|name| name == "homeboy.json") {
                continue;
            }
            let manifest = root.join(path);
            if let Some(component) =
                homeboy_core::component::discover_from_portable(manifest.parent().unwrap_or(&root))
            {
                components.push(component);
            }
        }
    }
    components
        .into_iter()
        .filter_map(|component| component.extensions)
        .flat_map(|linked| linked.into_keys())
        .collect()
}

/// Record the controller's install source and revision for each declared
/// extension it has installed, so a Lab gate host can provision the same
/// extension (#17769). Explicit inputs and recorded sources are kept.
pub(super) fn with_controller_extension_sources(
    policy: &AgentTaskGateEnvironmentPolicy,
    cwd: &Path,
) -> AgentTaskGateEnvironmentPolicy {
    let mut policy = policy.clone();
    if !policy.isolate_home {
        return policy;
    }
    for id in declared_component_extension_ids(policy.admitted_component_id.as_deref(), cwd) {
        if policy.extension_inputs.iter().any(|input| input.id == id)
            || policy
                .extension_sources
                .iter()
                .any(|source| source.id == id)
        {
            continue;
        }
        let Ok(dir) = homeboy_core::paths::extension(&id) else {
            continue;
        };
        if let Some(source) = homeboy_core::extension::lifecycle::read_source_url(&dir) {
            let revision = homeboy_core::extension::lifecycle::read_source_revision_at(&dir);
            policy.extension_sources.push(AgentTaskGateExtensionSource {
                id,
                source,
                revision,
            });
        }
    }
    policy
}
