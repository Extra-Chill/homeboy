//! Generic, dependency-aware release cascade.
//!
//! After an upstream component is released, the cascade releases every dependent
//! that declares a dependency on it: it updates the dependent's declared
//! dependency pin through a generic extension action ([`UPDATE_DEPENDENCY_ACTION`])
//! and then releases the dependent with an automatic patch bump.
//!
//! The cascade is framework-agnostic. Core carries only generic dependency
//! coordinates — the released component id, the package name that links the two
//! components, and the released version/tag/sha. Package-manager specifics (e.g.
//! rewriting a Composer custom-package's `dist.url`/`source.reference` or
//! regenerating a lockfile) live entirely in extensions, behind the
//! `release.update_dependency` action. No package-manager strings appear here.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::core::component::{self, Component};
use crate::core::deps::{self, DependencyStackPlanStep};
use crate::core::error::{Error, Result};
use crate::core::extension;
use crate::core::git;

use super::types::ReleaseCommandInput;

/// Generic extension action a dependent's extension implements to update its
/// declared pin on a just-released upstream. Receives the released coordinates
/// in the action payload's `dependency` block (see
/// [`build_update_dependency_payload`]).
pub const UPDATE_DEPENDENCY_ACTION: &str = "release.update_dependency";

/// Bump type used for a dependent released purely because an upstream changed.
/// Passing this as the release bump override also permits a release with no new
/// commits since the last tag (a pure dependency bump), instead of skipping
/// with "no releasable commits".
pub const DEPENDENCY_BUMP: &str = "patch";

/// Released coordinates for one component in the cascade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleasedCoordinates {
    pub component_id: String,
    pub version: String,
    pub tag: String,
    pub sha: String,
}

/// One incoming dependency edge into a downstream dependent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CascadeEdge {
    /// The upstream (released) component this edge depends on.
    pub upstream: String,
    /// The package name that links upstream → downstream.
    pub package: String,
}

/// A downstream dependent to update and release, with every incoming dependency
/// edge collapsed so the dependent is released at most once per cascade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CascadeTarget {
    pub downstream: String,
    pub downstream_path: String,
    pub incoming: Vec<CascadeEdge>,
}

/// Per-dependent outcome of a cascade run.
#[derive(Debug, Clone, Serialize)]
pub struct CascadeStepResult {
    pub downstream: String,
    /// Packages whose pin was updated on this dependent.
    pub updated_packages: Vec<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// Aggregate result of a cascade run.
#[derive(Debug, Clone, Serialize)]
pub struct CascadeResult {
    pub upstream: String,
    pub released: Vec<CascadeStepResult>,
}

/// Collapse an ordered (BFS / topological) dependency stack plan into a
/// deduplicated, first-seen-ordered list of downstream targets.
///
/// Each downstream appears once, with all of its incoming edges grouped, so a
/// dependent that consumes two cascaded upstreams is released a single time
/// rather than once per edge. Pure — performs no IO — so the dedupe/order
/// contract is unit-testable without a live dependency graph.
pub fn cascade_targets(steps: &[DependencyStackPlanStep]) -> Vec<CascadeTarget> {
    let mut order: Vec<String> = Vec::new();
    let mut by_downstream: BTreeMap<String, CascadeTarget> = BTreeMap::new();

    for step in steps {
        let target = by_downstream
            .entry(step.downstream.clone())
            .or_insert_with(|| {
                order.push(step.downstream.clone());
                CascadeTarget {
                    downstream: step.downstream.clone(),
                    downstream_path: step.downstream_path.clone(),
                    incoming: Vec::new(),
                }
            });
        let edge = CascadeEdge {
            upstream: step.upstream.clone(),
            package: step.package.clone(),
        };
        if !target.incoming.contains(&edge) {
            target.incoming.push(edge);
        }
    }

    order
        .into_iter()
        .filter_map(|id| by_downstream.remove(&id))
        .collect()
}

/// Build the generic payload handed to a dependent's
/// [`UPDATE_DEPENDENCY_ACTION`].
///
/// `release.{component_id,local_path}` target the *dependent* (so the action
/// runs against the dependent's checkout via `HOMEBOY_COMPONENT_PATH`), while
/// the `dependency` block carries the *upstream* release coordinates the
/// extension needs to repin: released component id, package name, version, tag,
/// and commit sha. The field names are package-manager-agnostic on purpose.
pub fn build_update_dependency_payload(
    downstream_id: &str,
    downstream_path: &str,
    package: &str,
    upstream: &ReleasedCoordinates,
) -> serde_json::Value {
    serde_json::json!({
        "release": {
            "component_id": downstream_id,
            "local_path": downstream_path,
            "version": upstream.version,
            "tag": upstream.tag,
        },
        "dependency": {
            "released_id": upstream.component_id,
            "package": package,
            "version": upstream.version,
            "tag": upstream.tag,
            "sha": upstream.sha,
        }
    })
}

/// Run the dependency-aware cascade for a just-released `root` component.
///
/// Enumerates dependents through the existing dependency stack graph, and for
/// each dependent (in topological order): updates every in-cascade upstream pin
/// via the generic extension action, then releases the dependent with an
/// automatic patch bump. The freshly released dependent's coordinates feed
/// transitive dependents deeper in the graph.
///
/// `base_input` supplies the shared release flags (apply, skip-checks,
/// git-identity, …) captured from the originating `homeboy release` invocation;
/// per-dependent fields (`component_id`, `bump_override`, `path_override`) are
/// overridden here.
pub fn run_cascade(
    root: &ReleasedCoordinates,
    base_input: &ReleaseCommandInput,
) -> Result<CascadeResult> {
    let components = component::list()?;
    let plan = deps::stack_plan_from_components(&root.component_id, &components)?;
    let targets = cascade_targets(&plan.planned_steps());

    let components_by_id: BTreeMap<String, Component> = components
        .into_iter()
        .map(|component| (component.id.clone(), component))
        .collect();

    let mut released: BTreeMap<String, ReleasedCoordinates> = BTreeMap::new();
    released.insert(root.component_id.clone(), root.clone());

    let mut steps = Vec::new();
    for target in targets {
        let mut updated_packages = Vec::new();
        for edge in &target.incoming {
            let Some(upstream) = released.get(&edge.upstream) else {
                // The upstream was not (yet) released in this cascade — skip the
                // edge rather than pin to a stale coordinate.
                continue;
            };
            update_dependency(&components_by_id, &target, &edge.package, upstream)?;
            updated_packages.push(edge.package.clone());
        }

        if updated_packages.is_empty() {
            continue;
        }

        let mut input = base_input.clone();
        input.component_id = target.downstream.clone();
        input.path_override = None;
        // A dependency-only release has no new commits; the patch override both
        // selects the bump and permits the otherwise-empty release.
        input.bump_override = Some(DEPENDENCY_BUMP.to_string());

        let (result, _exit) = super::run_command(input)?;

        if result.status == "released" {
            if let Some(version) = result.new_version.clone() {
                let sha = git::get_head_commit(&target.downstream_path).unwrap_or_default();
                released.insert(
                    target.downstream.clone(),
                    ReleasedCoordinates {
                        component_id: target.downstream.clone(),
                        version,
                        tag: result.tag.clone().unwrap_or_default(),
                        sha,
                    },
                );
            }
        }

        steps.push(CascadeStepResult {
            downstream: target.downstream.clone(),
            updated_packages,
            status: result.status,
            new_version: result.new_version,
            tag: result.tag,
        });
    }

    Ok(CascadeResult {
        upstream: root.component_id.clone(),
        released: steps,
    })
}

/// Update one dependent's pin on a released upstream via the dependent's
/// extension that implements [`UPDATE_DEPENDENCY_ACTION`].
fn update_dependency(
    components_by_id: &BTreeMap<String, Component>,
    target: &CascadeTarget,
    package: &str,
    upstream: &ReleasedCoordinates,
) -> Result<serde_json::Value> {
    let downstream = components_by_id.get(&target.downstream).ok_or_else(|| {
        Error::validation_invalid_argument(
            "cascade.downstream",
            format!(
                "Dependent '{}' is not a known component; cannot update its dependency on '{}'",
                target.downstream, upstream.component_id
            ),
            Some(target.downstream.clone()),
            None,
        )
    })?;

    let extension_id = find_update_dependency_extension(downstream)?;
    let payload =
        build_update_dependency_payload(&target.downstream, &target.downstream_path, package, upstream);

    extension::execute_action(
        &extension_id,
        UPDATE_DEPENDENCY_ACTION,
        None,
        None,
        Some(&payload),
    )
}

/// Find the dependent's extension that declares [`UPDATE_DEPENDENCY_ACTION`].
fn find_update_dependency_extension(downstream: &Component) -> Result<String> {
    let extensions = super::context::resolve_extensions(downstream)?;
    extensions
        .into_iter()
        .find(|manifest| {
            manifest
                .actions
                .iter()
                .any(|action| action.id == UPDATE_DEPENDENCY_ACTION)
        })
        .map(|manifest| manifest.id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cascade.update_dependency",
                format!(
                    "Dependent '{}' has no extension providing the '{}' action; cannot auto-update its dependency pin",
                    downstream.id, UPDATE_DEPENDENCY_ACTION
                ),
                Some(downstream.id.clone()),
                Some(vec![format!(
                    "Add an extension that implements the '{}' action to the dependent's homeboy.json",
                    UPDATE_DEPENDENCY_ACTION
                )]),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(
        sequence: usize,
        upstream: &str,
        downstream: &str,
        package: &str,
    ) -> DependencyStackPlanStep {
        DependencyStackPlanStep {
            sequence,
            declaring_component_id: downstream.to_string(),
            upstream: upstream.to_string(),
            downstream: downstream.to_string(),
            downstream_path: format!("/work/{downstream}"),
            package: package.to_string(),
            update_command: String::new(),
            rebuild: false,
            post_update: Vec::new(),
            test: Vec::new(),
        }
    }

    #[test]
    fn cascade_targets_preserve_topological_order() {
        let steps = vec![
            step(1, "php-transformer", "static-site-importer", "chubes/php-transformer"),
            step(2, "static-site-importer", "site-builder", "chubes/static-site-importer"),
        ];

        let targets = cascade_targets(&steps);

        let ids: Vec<_> = targets.iter().map(|t| t.downstream.as_str()).collect();
        assert_eq!(ids, vec!["static-site-importer", "site-builder"]);
        assert_eq!(targets[0].downstream_path, "/work/static-site-importer");
        assert_eq!(targets[0].incoming[0].upstream, "php-transformer");
        assert_eq!(targets[0].incoming[0].package, "chubes/php-transformer");
    }

    #[test]
    fn cascade_targets_collapse_a_diamond_into_one_release_per_dependent() {
        // a -> b, a -> c, b -> d, c -> d: `d` depends on both `b` and `c`.
        let steps = vec![
            step(1, "a", "b", "pkg/a"),
            step(2, "a", "c", "pkg/a"),
            step(3, "b", "d", "pkg/b"),
            step(4, "c", "d", "pkg/c"),
        ];

        let targets = cascade_targets(&steps);

        let ids: Vec<_> = targets.iter().map(|t| t.downstream.as_str()).collect();
        assert_eq!(ids, vec!["b", "c", "d"], "each dependent released once");

        let d = targets
            .iter()
            .find(|t| t.downstream == "d")
            .expect("d target");
        assert_eq!(
            d.incoming,
            vec![
                CascadeEdge {
                    upstream: "b".to_string(),
                    package: "pkg/b".to_string()
                },
                CascadeEdge {
                    upstream: "c".to_string(),
                    package: "pkg/c".to_string()
                },
            ],
            "both incoming edges are grouped onto the single dependent",
        );
    }

    #[test]
    fn cascade_targets_dedupe_identical_edges() {
        let steps = vec![
            step(1, "a", "b", "pkg/a"),
            step(2, "a", "b", "pkg/a"),
        ];

        let targets = cascade_targets(&steps);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].incoming.len(), 1);
    }

    #[test]
    fn update_dependency_payload_carries_generic_upstream_coordinates() {
        let upstream = ReleasedCoordinates {
            component_id: "php-transformer".to_string(),
            version: "1.4.0".to_string(),
            tag: "v1.4.0".to_string(),
            sha: "abc123def456".to_string(),
        };

        let payload = build_update_dependency_payload(
            "static-site-importer",
            "/work/static-site-importer",
            "chubes/php-transformer",
            &upstream,
        );

        // The action runs against the dependent's checkout.
        assert_eq!(payload["release"]["component_id"], "static-site-importer");
        assert_eq!(payload["release"]["local_path"], "/work/static-site-importer");

        // The dependency block carries everything an extension needs to repin.
        assert_eq!(payload["dependency"]["released_id"], "php-transformer");
        assert_eq!(payload["dependency"]["package"], "chubes/php-transformer");
        assert_eq!(payload["dependency"]["version"], "1.4.0");
        assert_eq!(payload["dependency"]["tag"], "v1.4.0");
        assert_eq!(payload["dependency"]["sha"], "abc123def456");
    }
}
