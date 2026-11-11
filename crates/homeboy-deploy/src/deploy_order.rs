//! Dependency-aware apply order for multi-component deploys.
//!
//! A project deploy writes components one at a time. When a component and the
//! components it depends on ship together, applying the dependent first leaves
//! the target briefly running new dependent code against an old dependency (a
//! WordPress plugin extending a class its required plugin had not shipped yet
//! fataled production requests for ~1s; homeboy#17899). This module orders the
//! apply set so a component is never applied before a dependency in the same
//! run. Dependencies come from the project attachment's `depends_on` and from
//! extension `deploy.dependency_inference` rules.

use std::collections::BTreeSet;
use std::path::Path;

use homeboy_core::error::{Error, Result};

/// One item in the apply set, described by the names it answers to and the
/// names it depends on.
#[derive(Debug, Clone, Default)]
pub(crate) struct OrderNode {
    pub(crate) id: String,
    /// Names a dependency entry may use for this item: id, local directory
    /// name, remote directory name.
    pub(crate) aliases: BTreeSet<String>,
    pub(crate) depends_on: Vec<String>,
}

impl OrderNode {
    pub(crate) fn new(id: &str, paths: &[&str], depends_on: Vec<String>) -> Self {
        let mut aliases = BTreeSet::from([id.to_string()]);
        for path in paths {
            let trimmed = path.trim().trim_end_matches('/');
            if let Some(name) = Path::new(trimmed).file_name().and_then(|n| n.to_str()) {
                if !name.is_empty() {
                    aliases.insert(name.to_string());
                }
            }
        }
        Self {
            id: id.to_string(),
            aliases,
            depends_on,
        }
    }
}

/// Indices of `nodes` in apply order.
///
/// Stable: with no dependencies the declared order is returned unchanged, and
/// among items whose dependencies are satisfied the earliest-declared goes
/// first. Dependencies naming nothing in the set are ignored. A cycle is an
/// error, returned before anything is applied.
pub(crate) fn dependency_order(nodes: &[OrderNode]) -> Result<Vec<usize>> {
    let edges: Vec<BTreeSet<usize>> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            node.depends_on
                .iter()
                .flat_map(|dependency| {
                    nodes
                        .iter()
                        .enumerate()
                        .filter(move |(other, candidate)| {
                            *other != index && candidate.aliases.contains(dependency.trim())
                        })
                        .map(|(other, _)| other)
                })
                .collect()
        })
        .collect();

    let mut placed = vec![false; nodes.len()];
    let mut order = Vec::with_capacity(nodes.len());
    while order.len() < nodes.len() {
        let next = (0..nodes.len())
            .find(|&index| !placed[index] && edges[index].iter().all(|&dep| placed[dep]));
        match next {
            Some(index) => {
                placed[index] = true;
                order.push(index);
            }
            None => {
                let stuck: Vec<&str> = (0..nodes.len())
                    .filter(|&index| !placed[index])
                    .map(|index| nodes[index].id.as_str())
                    .collect();
                return Err(Error::validation_invalid_argument(
                    "depends_on",
                    format!(
                        "Deploy dependency cycle between components: {}",
                        stuck.join(", ")
                    ),
                    None,
                    Some(vec![
                        "Remove the circular entry from the project attachment's depends_on or the component's declared dependencies".to_string(),
                    ]),
                ));
            }
        }
    }
    Ok(order)
}

/// Reorder `items` in place by [`dependency_order`] over `nodes` (same length
/// and index alignment). Returns whether the order changed.
pub(crate) fn reorder<T>(items: &mut Vec<T>, nodes: &[OrderNode]) -> Result<bool> {
    debug_assert_eq!(items.len(), nodes.len());
    let order = dependency_order(nodes)?;
    if order
        .iter()
        .enumerate()
        .all(|(position, &index)| position == index)
    {
        return Ok(false);
    }
    let mut slots: Vec<Option<T>> = items.drain(..).map(Some).collect();
    items.extend(order.into_iter().filter_map(|index| slots[index].take()));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, deps: &[&str]) -> OrderNode {
        OrderNode::new(id, &[], deps.iter().map(|d| d.to_string()).collect())
    }

    #[test]
    fn a_dependent_declared_first_is_applied_after_its_dependency() {
        // Alphabetical project order: events sorts before users.
        let nodes = vec![
            node("ai-adventure", &[]),
            node("events", &["data-machine", "users"]),
            node("newsletter", &[]),
            node("users", &[]),
        ];
        let order = dependency_order(&nodes).unwrap();
        let ids: Vec<&str> = order.iter().map(|&i| nodes[i].id.as_str()).collect();
        assert_eq!(ids, vec!["ai-adventure", "newsletter", "users", "events"]);
    }

    #[test]
    fn unrelated_components_keep_declared_order() {
        let nodes = vec![node("c", &[]), node("a", &[]), node("b", &["missing"])];
        assert_eq!(dependency_order(&nodes).unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn dependencies_match_directory_aliases() {
        let nodes = vec![
            OrderNode::new(
                "events",
                &[
                    "/src/plugins/extrachill-events",
                    "wp-content/plugins/extrachill-events/",
                ],
                vec!["extrachill-users".to_string()],
            ),
            OrderNode::new("users", &["/src/plugins/extrachill-users"], vec![]),
        ];
        assert_eq!(dependency_order(&nodes).unwrap(), vec![1, 0]);
    }

    #[test]
    fn a_cycle_is_an_error_naming_the_components() {
        let nodes = vec![node("a", &["b"]), node("b", &["a"]), node("c", &[])];
        let error = dependency_order(&nodes).unwrap_err().to_string();
        assert!(error.contains("cycle"), "{error}");
        assert!(error.contains("a, b"), "{error}");
    }

    #[test]
    fn reorder_moves_items_and_reports_change() {
        let mut items = vec!["events", "users"];
        let nodes = vec![node("events", &["users"]), node("users", &[])];
        assert!(reorder(&mut items, &nodes).unwrap());
        assert_eq!(items, vec!["users", "events"]);

        let mut unchanged = vec!["users", "events"];
        let nodes = vec![node("users", &[]), node("events", &["users"])];
        assert!(!reorder(&mut unchanged, &nodes).unwrap());
        assert_eq!(unchanged, vec!["users", "events"]);
    }
}
