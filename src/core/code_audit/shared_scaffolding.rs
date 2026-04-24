//! Shared-scaffolding (class-shape) detector.
//!
//! Finds groups of classes in the same directory subtree that share the same
//! overall method-shape signature (same method names + visibilities, in order)
//! AND have high body similarity across those methods. Such groups are
//! candidates for extraction into a shared base class.
//!
//! Where existing detectors compare pairs of functions, this detector compares
//! whole class shapes. It catches cases like data-machine's ~90 ability classes
//! all following `__construct → registerAbility → execute → checkPermission`.
//!
//! Algorithm:
//! 1. For each fingerprinted file with a class/type, compute a shape signature
//!    of `(method_name, visibility)` tuples, preserving method order.
//! 2. Group classes by (subtree_root, shape_signature).
//! 3. For each group with ≥ `MIN_GROUP_SIZE` classes, compute mean per-method
//!    body similarity using `method_hashes` (identical hash → 1.0, else 0.0).
//! 4. If mean similarity ≥ `MIN_MEAN_SIMILARITY`, emit one `Finding` per group
//!    describing the candidate base class and estimated LOC reduction.

use std::collections::HashMap;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

/// Minimum number of classes with identical shape to consider a group.
const MIN_GROUP_SIZE: usize = 3;

/// Minimum mean per-method body similarity (0.0 – 1.0).
const MIN_MEAN_SIMILARITY: f64 = 0.60;

/// Path depth used to bucket classes into subtrees. A file at
/// `inc/Abilities/AgentPing/SendPingAbility.php` has subtree root `inc/Abilities`.
const SUBTREE_DEPTH: usize = 2;

/// Approximate LOC per method body when projecting a base class.
/// Used only for the "estimated LOC reduction" annotation.
const AVG_METHOD_LOC: usize = 8;

pub(super) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    detect_shared_scaffolding(fingerprints)
}

/// A class (or class-like type) reduced to what this detector cares about.
struct ClassShape<'a> {
    fp: &'a FileFingerprint,
    /// Ordered (method_name, visibility) tuples — the shape signature.
    shape: Vec<(String, String)>,
    /// Subtree root (first N directory components).
    subtree: String,
}

fn detect_shared_scaffolding(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    // Build class shapes — only files that actually declare a class/type.
    let mut shapes: Vec<ClassShape> = Vec::new();
    for fp in fingerprints {
        if fp.type_name.is_none() {
            continue;
        }
        if fp.methods.is_empty() {
            continue;
        }
        let shape = build_shape_signature(fp);
        if shape.is_empty() {
            continue;
        }
        let subtree = subtree_root(&fp.relative_path);
        shapes.push(ClassShape { fp, shape, subtree });
    }

    // Group by (subtree, shape).
    let mut groups: HashMap<(String, Vec<(String, String)>), Vec<&ClassShape>> = HashMap::new();
    for cs in &shapes {
        groups
            .entry((cs.subtree.clone(), cs.shape.clone()))
            .or_default()
            .push(cs);
    }

    let mut findings = Vec::new();

    for ((subtree, shape), members) in &groups {
        if members.len() < MIN_GROUP_SIZE {
            continue;
        }

        let (mean_similarity, identical_methods) = mean_body_similarity(members, shape);
        if mean_similarity < MIN_MEAN_SIMILARITY {
            continue;
        }

        // Sort member file paths for deterministic output.
        let mut member_files: Vec<&str> = members
            .iter()
            .map(|m| m.fp.relative_path.as_str())
            .collect();
        member_files.sort();

        // Estimated LOC reduction: sum of method bodies across members, minus the
        // projected single base-class body. Approximate: AVG_METHOD_LOC per method.
        let total_methods = shape.len();
        let class_body_loc = total_methods * AVG_METHOD_LOC;
        let estimated_loc_reduction =
            class_body_loc.saturating_mul(members.len().saturating_sub(1));

        let method_list: Vec<String> = shape
            .iter()
            .map(|(name, vis)| {
                if vis.is_empty() {
                    name.clone()
                } else {
                    format!("{} {}", vis, name)
                }
            })
            .collect();

        let member_preview: String = if member_files.len() > 6 {
            let first: Vec<&str> = member_files.iter().take(5).copied().collect();
            format!("{} (+{} more)", first.join(", "), member_files.len() - 5)
        } else {
            member_files.join(", ")
        };

        let description = format!(
            "Shared scaffolding: {} classes under `{}` share shape `{}` \
             (mean body similarity {:.0}%, {} identical method bodies / {} total). \
             Members: {}. Estimated LOC reduction: ~{} lines.",
            members.len(),
            subtree,
            method_list.join(" → "),
            mean_similarity * 100.0,
            identical_methods,
            total_methods * members.len(),
            member_preview,
            estimated_loc_reduction,
        );

        let suggestion = format!(
            "Extract a shared base class under `{}` for `{}` and move the shared \
             method bodies into it. Subclasses can override only what actually varies.",
            subtree,
            method_list.join(", "),
        );

        findings.push(Finding {
            convention: "shared_scaffolding".to_string(),
            severity: Severity::Warning,
            file: subtree.clone(),
            description,
            suggestion,
            kind: AuditFinding::SharedScaffolding,
        });
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });
    findings
}

/// Build the ordered `(method_name, visibility)` shape signature for a file.
///
/// Methods preserve the order reported by the fingerprint pipeline. Visibility
/// defaults to empty string when absent — shape matching then treats "missing
/// visibility" as its own equivalence class, which is still safe because two
/// classes with missing visibility for the same method will still match each
/// other.
fn build_shape_signature(fp: &FileFingerprint) -> Vec<(String, String)> {
    fp.methods
        .iter()
        .map(|m| {
            let vis = fp.visibility.get(m).cloned().unwrap_or_default();
            (m.clone(), vis)
        })
        .collect()
}

/// Determine the subtree root (top-level directory under the component root)
/// for a given relative path.
///
/// Uses the first `SUBTREE_DEPTH` directory components. For `inc/Abilities/Chat/ChatAbility.php`,
/// this returns `inc/Abilities`. For shorter paths, falls back to the file's parent dir.
fn subtree_root(relative_path: &str) -> String {
    let normalized = relative_path.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    // parts includes the filename at the end.
    if parts.len() <= 1 {
        return ".".to_string();
    }
    let dir_depth = parts.len() - 1;
    let take = SUBTREE_DEPTH.min(dir_depth);
    if take == 0 {
        return ".".to_string();
    }
    parts[..take].join("/")
}

/// Compute the mean per-method body similarity across all members of a group.
///
/// For each method in the shape, we compare body hashes pairwise across members:
/// the fraction of member pairs with an identical hash contributes to that
/// method's similarity score. Missing hashes (when the fingerprint pipeline did
/// not populate `method_hashes` for a method) are treated as distinct, which
/// lowers similarity — matching the intent that we only fire when bodies are
/// genuinely similar.
///
/// Returns `(mean_similarity, identical_method_body_count)`. The identical
/// count is the number of (method, member) cells whose hash matches the most
/// common hash for that method — used only for human-readable reporting.
fn mean_body_similarity(members: &[&ClassShape], shape: &[(String, String)]) -> (f64, usize) {
    if members.is_empty() || shape.is_empty() {
        return (0.0, 0);
    }

    let mut sum = 0.0;
    let mut identical_cells = 0;

    for (method_name, _vis) in shape {
        // Collect body hashes for this method across all members.
        let hashes: Vec<Option<&str>> = members
            .iter()
            .map(|m| m.fp.method_hashes.get(method_name).map(|s| s.as_str()))
            .collect();

        // Count occurrences of each concrete hash; None is "missing" and does
        // not count toward any bucket.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for h in &hashes {
            if let Some(h) = h {
                *counts.entry(*h).or_insert(0) += 1;
            }
        }

        // Similarity for this method = size of the largest identical-hash bucket
        // divided by member count. All identical → 1.0. All distinct → 1/n.
        // All missing → 0.0.
        let max_bucket = counts.values().copied().max().unwrap_or(0);
        let method_similarity = max_bucket as f64 / members.len() as f64;
        sum += method_similarity;
        identical_cells += max_bucket;
    }

    let mean = sum / shape.len() as f64;
    (mean, identical_cells)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::Language;

    fn make_class(
        path: &str,
        type_name: &str,
        methods: &[(&str, &str, &str)], // (method, visibility, body_hash)
    ) -> FileFingerprint {
        let mut visibility = HashMap::new();
        let mut method_hashes = HashMap::new();
        let method_names: Vec<String> = methods
            .iter()
            .map(|(n, v, h)| {
                visibility.insert(n.to_string(), v.to_string());
                if !h.is_empty() {
                    method_hashes.insert(n.to_string(), h.to_string());
                }
                n.to_string()
            })
            .collect();

        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            methods: method_names,
            type_name: Some(type_name.to_string()),
            visibility,
            method_hashes,
            ..Default::default()
        }
    }

    #[test]
    fn fires_for_group_of_five_identical_shape_classes_with_high_similarity() {
        // Five classes sharing the same shape. `__construct` and
        // `registerAbility` have identical bodies across all five; `execute`
        // has two distinct bodies split 3/2. Mean similarity:
        //   __construct:     5/5 = 1.0
        //   registerAbility: 5/5 = 1.0
        //   execute:         3/5 = 0.6
        // mean = 2.6 / 3 ≈ 0.867 — well above the 0.60 threshold.
        let shape = &[
            ("__construct", "public", "ctor_h"),
            ("registerAbility", "private", "reg_h"),
            ("execute", "public", "exec_a"),
        ];
        let shape_alt_exec = &[
            ("__construct", "public", "ctor_h"),
            ("registerAbility", "private", "reg_h"),
            ("execute", "public", "exec_b"),
        ];

        let fps = vec![
            make_class("inc/Abilities/Chat/ChatAbility.php", "ChatAbility", shape),
            make_class(
                "inc/Abilities/AgentPing/SendPingAbility.php",
                "SendPingAbility",
                shape,
            ),
            make_class(
                "inc/Abilities/Engine/RunEngineAbility.php",
                "RunEngineAbility",
                shape,
            ),
            make_class(
                "inc/Abilities/Content/CreateContentAbility.php",
                "CreateContentAbility",
                shape_alt_exec,
            ),
            make_class(
                "inc/Abilities/File/UploadFileAbility.php",
                "UploadFileAbility",
                shape_alt_exec,
            ),
        ];
        let refs: Vec<&FileFingerprint> = fps.iter().collect();

        let findings = run(&refs);

        assert_eq!(
            findings.len(),
            1,
            "Expected exactly one shared-scaffolding finding, got {}: {:?}",
            findings.len(),
            findings.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
        let f = &findings[0];
        assert_eq!(f.kind, AuditFinding::SharedScaffolding);
        assert_eq!(f.file, "inc/Abilities");
        assert!(
            f.description.contains("5 classes"),
            "description should mention member count: {}",
            f.description
        );
        assert!(
            f.description.contains("__construct")
                && f.description.contains("registerAbility")
                && f.description.contains("execute"),
            "description should list shared method names: {}",
            f.description
        );
    }

    #[test]
    fn does_not_fire_for_matching_shape_with_zero_body_similarity() {
        // Three classes with matching shape but every single method body
        // differs across all members. Each method's largest identical bucket
        // is 1/3 ≈ 0.333, so the mean is well below 0.60.
        let mk = |path: &str, name: &str, hashes: [&str; 3]| {
            make_class(
                path,
                name,
                &[
                    ("__construct", "public", hashes[0]),
                    ("registerAbility", "private", hashes[1]),
                    ("execute", "public", hashes[2]),
                ],
            )
        };

        let fps = vec![
            mk(
                "inc/Abilities/A/AAbility.php",
                "AAbility",
                ["c1", "r1", "e1"],
            ),
            mk(
                "inc/Abilities/B/BAbility.php",
                "BAbility",
                ["c2", "r2", "e2"],
            ),
            mk(
                "inc/Abilities/C/CAbility.php",
                "CAbility",
                ["c3", "r3", "e3"],
            ),
        ];
        let refs: Vec<&FileFingerprint> = fps.iter().collect();

        let findings = run(&refs);
        assert!(
            findings.is_empty(),
            "Matching shape with zero body overlap should not fire: got {:?}",
            findings.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
    }

    #[test]
    fn does_not_fire_for_pair_below_group_threshold() {
        // Only two classes with matching shape and identical bodies — the
        // group-size threshold (3) blocks the finding even though similarity
        // is 1.0.
        let shape = &[
            ("__construct", "public", "ctor_h"),
            ("execute", "public", "exec_h"),
        ];

        let fps = vec![
            make_class("inc/Abilities/A/AAbility.php", "AAbility", shape),
            make_class("inc/Abilities/B/BAbility.php", "BAbility", shape),
        ];
        let refs: Vec<&FileFingerprint> = fps.iter().collect();

        let findings = run(&refs);
        assert!(
            findings.is_empty(),
            "A pair (2 members) should not fire — threshold is {}: got {:?}",
            MIN_GROUP_SIZE,
            findings.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
    }

    #[test]
    fn subtree_root_uses_first_two_components() {
        assert_eq!(
            subtree_root("inc/Abilities/Chat/ChatAbility.php"),
            "inc/Abilities"
        );
        assert_eq!(subtree_root("src/core/audit/mod.rs"), "src/core");
        assert_eq!(subtree_root("top_level.php"), ".");
        assert_eq!(subtree_root("just_dir/file.php"), "just_dir");
    }
}
