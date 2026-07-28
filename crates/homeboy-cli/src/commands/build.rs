use clap::Args;
use homeboy::core::component;
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::project;
use homeboy::core::scope::{self, Scope};
use homeboy_extension::build;
use homeboy_extension::ExtensionCapability;

use crate::commands::utils::args::ScopeArgs;
use crate::commands::utils::resolve::resolve_project_components;
use crate::commands::CmdResult;

#[derive(Args)]
pub struct BuildArgs {
    /// JSON input spec for bulk operations: {"componentIds": ["id1", "id2"]}
    #[arg(long)]
    pub json: Option<String>,

    /// Target ID: component ID or project ID (when using --all)
    pub target_id: Option<String>,

    /// Additional component IDs (enables project/component order detection)
    pub component_ids: Vec<String>,

    /// Build all components in the project
    #[arg(long)]
    pub all: bool,

    // Entity selection. `--path` keeps its historical build meaning — override
    // local_path for this build (a workspace clone or temp checkout) — so it
    // still composes with the positional target and CWD discovery below. The
    // registry selectors (--component/--project/--fleet/--rig/--workspace) are
    // resolved through the shared scope resolver instead.
    #[command(flatten)]
    pub scope: ScopeArgs,

    /// Ask the build provider to resolve the build scope from files changed since this git ref
    #[arg(long)]
    pub changed_since: Option<String>,
}

pub fn run(
    args: BuildArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<build::BuildResult> {
    // Priority: --json > --all with project > positional args

    // Shared tail: every multi-component build path dispatches the resolved
    // component records through the same changed-since runner with the same
    // `--changed-since` argument, so funnel them through one helper.
    let run_components = |components: &[component::Component]| {
        build::run_components_with_changed_since(components, args.changed_since.as_deref())
    };

    // JSON takes precedence
    if let Some(ref json) = args.json {
        return build::run(json);
    }

    // An explicit registry selector resolves through the shared scope
    // resolver. `Scope::Path` is deliberately excluded: `--path` on build is a
    // local_path override for the positional/CWD paths below, not a standalone
    // entity selector, and rerouting it here would change what `--path` means.
    match args.scope.selection() {
        None | Some(Scope::Path { .. }) => {}
        Some(selected) => {
            let components = scope::resolve_scope_component_records(&selected)?;
            return run_components(&components);
        }
    }

    // No target_id: try CWD auto-discovery (registered component or homeboy.json)
    if args.target_id.is_none() && args.component_ids.is_empty() && !args.all {
        let ctx = execution_context::resolve(&ResolveOptions::with_capability(
            // Use empty string for CWD auto-discovery — resolve_effective handles this
            component::resolve(None)?.id.as_str(),
            args.scope.path.clone(),
            ExtensionCapability::Build,
            Vec::new(),
        ))?;
        return build::run_component_with_changed_since(
            &ctx.component,
            args.changed_since.as_deref(),
        );
    }

    let target_id = args.target_id.as_ref().ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "input",
            "Provide component ID, project ID with --all, or JSON spec",
            None,
            Some(vec![
                "Build a single component: homeboy review build <component-id>".to_string(),
                "Build all project components: homeboy review build <project-id> --all".to_string(),
            ]),
        )
    })?;

    // --all mode: build all components in project
    if args.all {
        let proj = project::load(target_id).map_err(|e| {
            homeboy::core::Error::validation_invalid_argument(
                "project_id",
                format!("'{}' is not a valid project ID", target_id),
                None,
                Some(vec![
                    format!("Error: {}", e),
                    "Use --all only with a project ID: homeboy review build <project-id> --all"
                        .to_string(),
                ]),
            )
        })?;

        if proj.components.is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "project_id",
                format!("Project '{}' has no components configured", target_id),
                None,
                Some(vec![format!(
                    "Add components: homeboy project components add {} <component-id> or attach a repo: homeboy project components attach-path {} <component-id> <path>",
                    target_id,
                    target_id
                )]),
            ));
        }

        let components =
            scope::resolve_scope_component_records(&Scope::Project(target_id.clone()))?;
        return run_components(&components);
    }

    // Multiple positional args: use shared resolver
    if !args.component_ids.is_empty() {
        let (project_id, component_ids) =
            resolve_project_components(target_id, &args.component_ids)?;

        // Validate all components belong to this project
        let proj = project::load(&project_id)?;
        let invalid: Vec<_> = component_ids
            .iter()
            .filter(|c| !project::has_component(&proj, c))
            .collect();

        if !invalid.is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "component_ids",
                format!(
                    "Components not in project '{}': {}",
                    project_id,
                    invalid
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None,
                Some(vec![format!(
                    "Project components: {}",
                    project::project_component_ids(&proj).join(", ")
                )]),
            ));
        }

        let project_components =
            scope::resolve_scope_component_records(&Scope::Project(project_id.clone()))?;
        let components: Vec<_> = component_ids
            .iter()
            .filter_map(|id| {
                project_components
                    .iter()
                    .find(|component| component.id == *id)
                    .cloned()
            })
            .collect();

        return run_components(&components);
    }

    // Single target_id: treat as component ID
    if let Some(ref path) = args.scope.path {
        build::run_with_path_changed_since(target_id, path, args.changed_since.as_deref())
    } else {
        build::run_changed_since(target_id, args.changed_since.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::BuildArgs;
    use clap::Parser;
    use homeboy::core::scope::Scope;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: BuildArgs,
    }

    fn parse(argv: &[&str]) -> BuildArgs {
        TestCli::try_parse_from(argv)
            .expect("build invocation should parse")
            .args
    }

    /// Every shape `homeboy build` accepted before `ScopeArgs` must still
    /// parse into the same fields. `--path` in particular keeps its
    /// local_path-override meaning; it only moved into the shared group.
    #[test]
    fn previously_valid_invocations_still_parse() {
        let single = parse(&["build", "my-comp"]);
        assert_eq!(single.target_id.as_deref(), Some("my-comp"));
        assert!(single.component_ids.is_empty());
        assert!(!single.all);
        assert!(single.scope.path.is_none());

        let project_all = parse(&["build", "my-project", "--all"]);
        assert_eq!(project_all.target_id.as_deref(), Some("my-project"));
        assert!(project_all.all);

        let multi = parse(&["build", "my-project", "comp-a", "comp-b"]);
        assert_eq!(multi.target_id.as_deref(), Some("my-project"));
        assert_eq!(multi.component_ids, vec!["comp-a", "comp-b"]);

        let with_path = parse(&["build", "my-comp", "--path", "/tmp/checkout"]);
        assert_eq!(with_path.target_id.as_deref(), Some("my-comp"));
        assert_eq!(with_path.scope.path.as_deref(), Some("/tmp/checkout"));

        let bare_path = parse(&["build", "--path", "/tmp/checkout"]);
        assert!(bare_path.target_id.is_none());
        assert_eq!(bare_path.scope.path.as_deref(), Some("/tmp/checkout"));

        let json = parse(&["build", "--json", r#"{"componentIds":["a"]}"#]);
        assert_eq!(json.json.as_deref(), Some(r#"{"componentIds":["a"]}"#));

        let changed = parse(&["build", "my-comp", "--changed-since", "origin/main"]);
        assert_eq!(changed.changed_since.as_deref(), Some("origin/main"));

        let cwd = parse(&["build"]);
        assert!(cwd.target_id.is_none());
        assert!(cwd.scope.is_unscoped());
    }

    /// `--path` stays out of the shared-resolver branch so it keeps composing
    /// with the positional target and CWD discovery.
    #[test]
    fn path_still_reads_as_a_local_path_override() {
        let args = parse(&["build", "my-comp", "--path", "/tmp/checkout"]);
        assert_eq!(
            args.scope.selection(),
            Some(Scope::Path {
                path: "/tmp/checkout".to_string(),
                component_id: None,
            })
        );
        assert_eq!(args.target_id.as_deref(), Some("my-comp"));
    }

    #[test]
    fn registry_selectors_resolve_to_their_scope_variants() {
        assert_eq!(
            parse(&["build", "--component", "my-comp"])
                .scope
                .selection(),
            Some(Scope::Component("my-comp".to_string()))
        );
        assert_eq!(
            parse(&["build", "--project", "my-project"])
                .scope
                .selection(),
            Some(Scope::Project("my-project".to_string()))
        );
        assert_eq!(
            parse(&["build", "--fleet", "growth"]).scope.selection(),
            Some(Scope::Fleet("growth".to_string()))
        );
        assert_eq!(
            parse(&["build", "--rig", "studio"]).scope.selection(),
            Some(Scope::Rig("studio".to_string()))
        );
        assert_eq!(
            parse(&["build", "--workspace"]).scope.selection(),
            Some(Scope::Workspace)
        );
    }

    #[test]
    fn scope_selectors_conflict_with_each_other() {
        for argv in [
            vec!["build", "--component", "a", "--project", "b"],
            vec!["build", "--component", "a", "--path", "/tmp/a"],
            vec!["build", "--project", "a", "--workspace"],
            vec!["build", "--fleet", "a", "--rig", "b"],
        ] {
            assert!(
                TestCli::try_parse_from(&argv).is_err(),
                "conflicting build scopes should be rejected: {argv:?}"
            );
        }
    }
}
