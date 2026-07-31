//! Provider-neutral dependency graph and readiness projection for durable fanout.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

use homeboy_core::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskDependencyNode {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracker_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskDependencyEdge {
    pub upstream_id: String,
    pub downstream_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskDependencyState {
    Queued,
    Ready,
    BlockedByDependency,
    BlockedByGate,
    AwaitingAcceptance,
    Succeeded,
    Rejected,
    Failed,
    Cancelled,
}

impl AgentTaskDependencyState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Rejected | Self::Failed | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskDependencyReadiness {
    pub states: BTreeMap<String, AgentTaskDependencyState>,
    pub ready: Vec<String>,
    pub blocked_paths: BTreeMap<String, Vec<String>>,
    pub next_action: String,
}

/// Validate durable child identities and project executable siblings. This is a
/// generic read-side seam: callers own candidate, gate, and PR mutation.
pub fn dependency_graph_readiness(
    nodes: &[AgentTaskDependencyNode],
    states: &BTreeMap<String, AgentTaskDependencyState>,
) -> Result<(Vec<AgentTaskDependencyEdge>, AgentTaskDependencyReadiness)> {
    let mut by_id = BTreeMap::new();
    let mut tracker_ids = BTreeMap::new();
    for node in nodes {
        if node.id.trim().is_empty() {
            return Err(graph_error("child id must not be empty"));
        }
        if by_id.insert(node.id.as_str(), node).is_some() {
            return Err(graph_error(&format!("duplicate child id '{}'", node.id)));
        }
        if let Some(url) = node.tracker_url.as_deref() {
            if tracker_ids.insert(url, node.id.as_str()).is_some() {
                return Err(graph_error(&format!("duplicate tracker URL '{url}'")));
            }
        }
    }

    let mut edges = Vec::new();
    for node in nodes {
        for reference in &node.depends_on {
            let upstream = by_id
                .get(reference.as_str())
                .copied()
                .or_else(|| {
                    tracker_ids
                        .get(reference.as_str())
                        .and_then(|id| by_id.get(id).copied())
                })
                .ok_or_else(|| {
                    graph_error(&format!(
                        "child '{}' depends on missing or ambiguous child/tracker '{}'",
                        node.id, reference
                    ))
                })?;
            if upstream.repository != node.repository {
                return Err(graph_error(&format!(
                    "cross-repository edge '{}' -> '{}' is unsupported",
                    upstream.id, node.id
                )));
            }
            edges.push(AgentTaskDependencyEdge {
                upstream_id: upstream.id.clone(),
                downstream_id: node.id.clone(),
            });
        }
    }
    edges.sort_by(|a, b| {
        (&a.downstream_id, &a.upstream_id).cmp(&(&b.downstream_id, &b.upstream_id))
    });
    detect_cycle(nodes, &edges)?;

    let mut projected = BTreeMap::new();
    let mut ready = Vec::new();
    let mut blocked_paths = BTreeMap::new();
    for node in nodes {
        let current = states
            .get(&node.id)
            .copied()
            .unwrap_or(AgentTaskDependencyState::Queued);
        let dependencies = edges
            .iter()
            .filter(|edge| edge.downstream_id == node.id)
            .collect::<Vec<_>>();
        let failed = dependencies.iter().find(|edge| {
            matches!(
                states.get(&edge.upstream_id).copied(),
                Some(
                    AgentTaskDependencyState::Rejected
                        | AgentTaskDependencyState::Failed
                        | AgentTaskDependencyState::Cancelled
                )
            )
        });
        let pending = dependencies.iter().find(|edge| {
            states
                .get(&edge.upstream_id)
                .copied()
                .unwrap_or(AgentTaskDependencyState::Queued)
                != AgentTaskDependencyState::Succeeded
        });
        let state = if current.is_terminal()
            || matches!(
                current,
                AgentTaskDependencyState::BlockedByGate
                    | AgentTaskDependencyState::AwaitingAcceptance
            ) {
            current
        } else if let Some(edge) = failed {
            blocked_paths.insert(
                node.id.clone(),
                vec![node.id.clone(), edge.upstream_id.clone()],
            );
            AgentTaskDependencyState::BlockedByDependency
        } else if let Some(edge) = pending {
            blocked_paths.insert(
                node.id.clone(),
                vec![node.id.clone(), edge.upstream_id.clone()],
            );
            AgentTaskDependencyState::BlockedByDependency
        } else {
            ready.push(node.id.clone());
            AgentTaskDependencyState::Ready
        };
        projected.insert(node.id.clone(), state);
    }
    let next_action = if let Some(id) = ready.first() {
        format!("dispatch ready child '{id}'")
    } else if let Some((id, path)) = blocked_paths.iter().next() {
        format!(
            "resolve dependency path {} before dispatching '{id}'",
            path.join(" <- ")
        )
    } else {
        "inspect terminal child evidence and accept or retry the next candidate".to_string()
    };
    Ok((
        edges,
        AgentTaskDependencyReadiness {
            states: projected,
            ready,
            blocked_paths,
            next_action,
        },
    ))
}

fn detect_cycle(
    nodes: &[AgentTaskDependencyNode],
    edges: &[AgentTaskDependencyEdge],
) -> Result<()> {
    let mut incoming = nodes
        .iter()
        .map(|node| (node.id.clone(), 0usize))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing = BTreeMap::<String, Vec<String>>::new();
    for edge in edges {
        *incoming
            .get_mut(&edge.downstream_id)
            .expect("validated node") += 1;
        outgoing
            .entry(edge.upstream_id.clone())
            .or_default()
            .push(edge.downstream_id.clone());
    }
    let mut queue = incoming
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(id, _)| id.clone())
        .collect::<VecDeque<_>>();
    let mut visited = 0;
    while let Some(id) = queue.pop_front() {
        visited += 1;
        for downstream in outgoing.get(&id).into_iter().flatten() {
            let count = incoming.get_mut(downstream).expect("validated node");
            *count -= 1;
            if *count == 0 {
                queue.push_back(downstream.clone());
            }
        }
    }
    if visited != nodes.len() {
        let cycle = incoming
            .into_iter()
            .filter_map(|(id, count)| (count > 0).then_some(id))
            .take(8)
            .collect::<Vec<_>>();
        return Err(graph_error(&format!(
            "dependency cycle detected among: {}",
            cycle.join(", ")
        )));
    }
    Ok(())
}

fn graph_error(message: &str) -> Error {
    Error::validation_invalid_argument("dependencies", message, None, Some(vec!["Use unique child IDs or tracker URLs, and declare dependencies only within one repository.".to_string()]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, depends_on: &[&str]) -> AgentTaskDependencyNode {
        AgentTaskDependencyNode {
            id: id.into(),
            tracker_url: Some(format!("https://tracker/{id}")),
            repository: Some("homeboy".into()),
            worktree: Some(format!("/tmp/{id}")),
            head: Some(format!("feature/{id}")),
            depends_on: depends_on.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn schedules_independent_siblings_then_their_dependent() {
        let nodes = vec![
            node("foundation", &[]),
            node("sibling", &[]),
            node("dependent", &["https://tracker/foundation"]),
        ];
        let (edges, initial) =
            dependency_graph_readiness(&nodes, &BTreeMap::new()).expect("valid graph");
        assert_eq!(edges.len(), 1);
        assert_eq!(initial.ready, vec!["foundation", "sibling"]);
        assert_eq!(
            initial.states["dependent"],
            AgentTaskDependencyState::BlockedByDependency
        );
        let states = BTreeMap::from([
            ("foundation".into(), AgentTaskDependencyState::Succeeded),
            ("sibling".into(), AgentTaskDependencyState::Succeeded),
        ]);
        let (_, ready) = dependency_graph_readiness(&nodes, &states).expect("ready dependent");
        assert_eq!(ready.ready, vec!["dependent"]);
    }

    #[test]
    fn rejects_cycles_and_cross_repository_edges_before_dispatch() {
        let cycle = vec![node("a", &["b"]), node("b", &["a"])];
        assert!(dependency_graph_readiness(&cycle, &BTreeMap::new())
            .unwrap_err()
            .message
            .contains("cycle"));
        let mut other = node("other", &["a"]);
        other.repository = Some("other".into());
        assert!(
            dependency_graph_readiness(&[node("a", &[]), other], &BTreeMap::new())
                .unwrap_err()
                .message
                .contains("cross-repository")
        );
    }
}
