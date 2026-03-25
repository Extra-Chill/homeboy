//! call_graph_analysis — extracted from decompose.rs.

use super::super::move_items::{MoveOptions, MoveResult};
use super::super::*;
use super::find;
use super::is_stop_word;
use super::item;
use super::truncate_module_name;
use super::union;
use super::A;
use super::HUB_THRESHOLD;
use crate::extension::{self, ParsedItem};
use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Build a map of which functions call which other functions in the same file.
///
/// Returns: caller -> set of callees (only functions that exist in `fn_names`).
pub(crate) fn build_call_graph(
    fn_items: &[&ParsedItem],
    fn_names: &HashSet<&str>,
) -> BTreeMap<String, HashSet<String>> {
    let mut graph: BTreeMap<String, HashSet<String>> = BTreeMap::new();

    for item in fn_items {
        let mut callees = HashSet::new();
        for name in fn_names {
            if *name != item.name && item.source.contains(name) {
                // Verify it's a real call reference (not just a substring match)
                // by checking for word boundaries around the name
                let pattern = format!(r"\b{}\b", regex::escape(name));
                if let Ok(re) = regex::Regex::new(&pattern) {
                    if re.is_match(&item.source) {
                        callees.insert(name.to_string());
                    }
                }
            }
        }
        graph.insert(item.name.clone(), callees);
    }

    graph
}

/// Find connected components in the call graph using hub-aware union-find.
///
/// Unlike naive union-find, this identifies "hub" functions — orchestrators
/// that call many others — and excludes their edges from clustering. This
/// prevents mega-groups where everything reachable from a hub collapses into
/// one component.
///
/// Hub functions are returned separately so they can be kept in the parent
/// module (they're the orchestration layer that ties sub-clusters together).
///
/// Returns: (clustered groups, hub function names)
pub(crate) fn call_graph_components(
    graph: &BTreeMap<String, HashSet<String>>,
) -> (Vec<(String, Vec<String>)>, Vec<String>) {
    let all_names: Vec<String> = graph.keys().cloned().collect();

    // Identify hubs: functions that call HUB_THRESHOLD or more other functions
    let hubs: HashSet<&str> = graph
        .iter()
        .filter(|(_, callees)| callees.len() >= HUB_THRESHOLD)
        .map(|(name, _)| name.as_str())
        .collect();

    // Union-find on non-hub functions only
    let non_hub_names: Vec<&String> = all_names
        .iter()
        .filter(|n| !hubs.contains(n.as_str()))
        .collect();
    let mut parent: BTreeMap<String, String> = BTreeMap::new();
    for name in &non_hub_names {
        parent.insert((*name).clone(), (*name).clone());
    }

    fn find(parent: &mut BTreeMap<String, String>, x: &str) -> String {
        let p = parent.get(x).cloned().unwrap_or_else(|| x.to_string());
        if p != x {
            let root = find(parent, &p);
            parent.insert(x.to_string(), root.clone());
            root
        } else {
            p
        }
    }

    fn union(parent: &mut BTreeMap<String, String>, a: &str, b: &str) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent.insert(rb, ra);
        }
    }

    // Build edges only between non-hub functions.
    // Also consider reverse edges from hub callees: if a hub calls both A and B,
    // and A also calls B, then A and B should still cluster together.
    for (caller, callees) in graph {
        if hubs.contains(caller.as_str()) {
            continue; // Skip hub edges
        }
        for callee in callees {
            if parent.contains_key(callee) {
                union(&mut parent, caller, callee);
            }
        }
    }

    // Group by root
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in &non_hub_names {
        let root = find(&mut parent, name);
        groups.entry(root).or_default().push((*name).clone());
    }

    // Only return groups with 2+ members (singletons aren't useful)
    let clustered: Vec<(String, Vec<String>)> = groups
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .collect();

    let hub_names: Vec<String> = hubs.iter().map(|s| s.to_string()).collect();
    (clustered, hub_names)
}

/// Pick a representative name for a call-graph cluster.
///
/// Uses a multi-signal heuristic:
/// 1. **Shared prefix** — if most members share a prefix (e.g., `resolve_*`), use it
/// 2. **Longest common prefix** — if all members share a 2+ word prefix, use it
/// 3. **Most-called function** — fall back to the function called most by others
/// 4. **First alphabetically** — final fallback
pub(crate) fn pick_cluster_label(
    members: &[String],
    graph: &BTreeMap<String, HashSet<String>>,
) -> String {
    // Strategy 1: Check if most members share a common prefix
    if let Some(prefix) = find_dominant_prefix(members) {
        return prefix;
    }

    // Strategy 2: Most-called function as label
    let mut call_count: BTreeMap<&str, usize> = BTreeMap::new();
    for member in members {
        for callee in graph.get(member).into_iter().flatten() {
            if members.iter().any(|m| m == callee) {
                *call_count.entry(callee.as_str()).or_default() += 1;
            }
        }
    }

    let raw = call_count
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| {
            members
                .first()
                .cloned()
                .unwrap_or_else(|| "group".to_string())
        });

    truncate_module_name(&raw)
}

/// Find a dominant prefix shared by most members of a cluster.
///
/// Returns the prefix if ≥60% of members share it and it's a meaningful
/// word (not a stop word). Prefers longer (more specific) prefixes.
pub(crate) fn find_dominant_prefix(members: &[String]) -> Option<String> {
    if members.len() < 2 {
        return None;
    }

    let threshold = (members.len() as f64 * 0.6).ceil() as usize;
    let mut prefix_counts: BTreeMap<String, usize> = BTreeMap::new();

    for member in members {
        let parts: Vec<&str> = member.split('_').filter(|s| !s.is_empty()).collect();
        // Try 2-word prefix first, then 1-word
        if parts.len() >= 2 {
            let two_word = format!("{}_{}", parts[0], parts[1]).to_lowercase();
            if !is_stop_word(parts[0]) {
                *prefix_counts.entry(two_word).or_default() += 1;
            }
        }
        if !parts.is_empty() && !is_stop_word(parts[0]) && parts[0].len() > 2 {
            *prefix_counts.entry(parts[0].to_lowercase()).or_default() += 1;
        }
    }

    // Find the longest prefix that meets the threshold
    let mut candidates: Vec<_> = prefix_counts
        .into_iter()
        .filter(|(_, count)| *count >= threshold)
        .collect();

    // Sort by specificity: longer prefix first, then by count
    candidates.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| b.1.cmp(&a.1)));

    candidates.into_iter().next().map(|(prefix, _)| prefix)
}
