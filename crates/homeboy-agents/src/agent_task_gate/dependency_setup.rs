use super::*;

/// Ask the generic dependency-provider resolver about the checkout root and its
/// non-ignored descendant directories. This layer does not inspect manifests, infer ecosystems, or
/// construct install commands.
///
/// `gate_commands` holds the text of every gate this setup is for (each argv
/// joined by spaces). A descendant root is hydrated only when a gate names its
/// checkout-relative path. `None` is for setup that serves no particular gate
/// (a provider's attempt workspace) and hydrates every descendant root.
pub(crate) fn hydrate_gate_dependency_roots_for_component(
    checkout: &Path,
    enabled: bool,
    workspace: &str,
    policy: &homeboy_core::deps::DependencyHydrationPolicy,
    component_id: Option<&str>,
    gate_commands: Option<&[String]>,
) -> Result<Vec<AgentTaskGateSetupEvidence>> {
    if !enabled {
        return Ok(Vec::new());
    }
    // One timing report covers the whole walk; per-root outcomes stay in the
    // returned setup evidence (#18027).
    homeboy_engine_primitives::phase_timing::PhaseTimer::current()
        .time_with_outcome::<_, homeboy_core::Error>(
            "stage.dependency_hydration",
            || walk_gate_dependency_roots(checkout, workspace, policy, component_id, gate_commands),
            |outcomes| {
                outcomes.iter().any(|outcome| {
                    outcome.status == homeboy_core::deps::DependencyHydrationStatus::Failed
                })
            },
        )
}

pub(super) fn walk_gate_dependency_roots(
    checkout: &Path,
    workspace: &str,
    policy: &homeboy_core::deps::DependencyHydrationPolicy,
    component_id: Option<&str>,
    gate_commands: Option<&[String]>,
) -> Result<Vec<AgentTaskGateSetupEvidence>> {
    let component = match homeboy_core::deps::resolve_dependency_hydration_component(
        checkout,
        workspace,
        ".",
        policy,
        component_id,
    )? {
        homeboy_core::deps::DependencyHydrationComponent::Resolved(component) => component,
        homeboy_core::deps::DependencyHydrationComponent::Unavailable => return Ok(Vec::new()),
        homeboy_core::deps::DependencyHydrationComponent::DeadlineExceeded(outcome) => {
            return Ok(vec![outcome]);
        }
    };
    let mut candidates = vec![checkout.to_path_buf()];
    let mut pending = vec![checkout.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut children = Vec::new();
        for entry in fs::read_dir(&directory).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read candidate dependency roots".to_string()),
            )
        })? {
            let entry = entry.map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("read candidate dependency root".to_string()),
                )
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("read candidate dependency root type".to_string()),
                )
            })?;
            // A linked directory can escape the detached candidate checkout. Setup
            // is allowed only in real directories beneath the checkout.
            if file_type.is_dir()
                && path.file_name().is_none_or(|name| name != ".git")
                && !homeboy_core::deps::is_nested_repository(&path)
            {
                children.push(path);
            }
        }
        // Prune ignored output before descending: generated manifests are not
        // dependency roots, and dependency trees can contain thousands of dirs.
        let ignored = git_ignored_children(checkout, &children);
        for child in children {
            if !ignored.contains(&child) {
                pending.push(child.clone());
                candidates.push(child);
            }
        }
    }
    candidates.sort();
    let mut evidence = Vec::new();
    // Candidates sort before their descendants, so an ancestor selection root
    // hydrates before any directory that resolves to it. A descendant whose
    // selections all name an already-hydrated root is that root's own install
    // seen from below — an upward adapter search matched the ancestor's
    // lockfile — and hydrating it again would rerun the ancestor's install
    // from inside a workspace member. A selection root that produced no
    // hydration (its providers matched nothing) leaves its descendants to
    // hydrate themselves.
    let mut hydrated_roots: HashSet<PathBuf> = HashSet::new();
    for candidate in candidates {
        let relative = checkout_relative_path(checkout, &candidate);
        // Gate setup hydrates a descendant root only when a gate names it, as
        // in `cd php-transformer && composer run lint` (#16109). Any other
        // tracked lockfile is an install the gates do not need and one more
        // way for setup to fail (#16857).
        if candidate != checkout
            && gate_commands
                .is_some_and(|commands| !commands.iter().any(|command| command.contains(&relative)))
        {
            continue;
        }
        let relative = if relative.is_empty() { "." } else { &relative };
        let selections = homeboy_core::deps::dependency_selection_roots(&candidate, true)?;
        // An installed adapter's default fallback declares the walk's own root
        // only. A descendant that only a default selects (a fixture's bare
        // package.json, a workspace member) is not a dependency root (#16670);
        // descendants no adapter selects still reach their other providers.
        if candidate != checkout
            && !selections.is_empty()
            && homeboy_core::deps::dependency_selection_roots(&candidate, false)?.is_empty()
        {
            continue;
        }
        if !selections.is_empty()
            && selections
                .iter()
                .all(|selection_root| hydrated_roots.contains(selection_root))
        {
            continue;
        }
        let mut outcomes = homeboy_core::deps::hydrate_declared_dependencies_with_component(
            &component, &candidate, workspace, relative, policy,
        )?;
        let failed = outcomes
            .iter()
            .any(|outcome| outcome.status == homeboy_core::deps::DependencyHydrationStatus::Failed);
        if !outcomes.is_empty() {
            hydrated_roots.insert(candidate);
        }
        evidence.append(&mut outcomes);
        if failed {
            break;
        }
    }
    evidence.sort_by(|left, right| {
        left.package_root
            .cmp(&right.package_root)
            .then_with(|| left.provider_id.cmp(&right.provider_id))
    });
    Ok(evidence)
}

/// `candidate`'s path from `checkout` as a `/`-separated string; empty for the
/// checkout itself.
pub(super) fn checkout_relative_path(checkout: &Path, candidate: &Path) -> String {
    candidate
        .strip_prefix(checkout)
        .unwrap_or(candidate)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The `children` git ignores in `checkout`. Outside a git checkout, or when
/// git cannot answer, nothing counts as ignored.
pub(super) fn git_ignored_children(checkout: &Path, children: &[PathBuf]) -> Vec<PathBuf> {
    use std::io::Write;
    if children.is_empty() {
        return Vec::new();
    }
    let Ok(mut child) = std::process::Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["check-ignore", "--stdin", "-z"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    // Ask about each child by its path from the checkout root, not its bare
    // name: a nested `.gitignore` (`extensions/.gitignore: **/node_modules/`)
    // only applies to paths beneath it, and a root-anchored pattern (`/build/`)
    // must not prune a same-named directory deeper in the tree (#16857).
    let mut input = Vec::new();
    for path in children {
        if let Ok(relative) = path.strip_prefix(checkout) {
            input.extend_from_slice(relative.as_os_str().as_encoded_bytes());
            // A trailing slash makes directory-only patterns (`dist/`) match.
            input.extend_from_slice(b"/\0");
        }
    }
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(&input).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Vec::new();
        }
    }
    let Ok(output) = child.wait_with_output() else {
        return Vec::new();
    };
    // 0: some paths ignored, 1: none, anything else: not a checkout or an error.
    if output.status.code() != Some(0) {
        return Vec::new();
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| {
            let name = String::from_utf8_lossy(name);
            checkout.join(name.trim_end_matches('/'))
        })
        .collect()
}
