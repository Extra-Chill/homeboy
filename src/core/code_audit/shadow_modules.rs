//! Shadow module detection — find directories that are near-copies of each other.
//!
//! When two directories share 50%+ of file names and those files have high
//! content similarity, one is likely a copy-paste of the other that was never
//! consolidated. This is a stronger signal than individual DuplicateFunction
//! findings because it indicates systemic duplication — an entire module was
//! copied instead of properly refactored.
//!
//! Language-agnostic: operates on file names and structural hashes from
//! fingerprinting. No language parsing.

use std::collections::{HashMap, HashSet};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

/// Minimum file name overlap ratio to consider two directories related.
const MIN_NAME_OVERLAP: f64 = 0.5;

/// Minimum method hash overlap ratio to confirm shadow module status.
const MIN_CONTENT_OVERLAP: f64 = 0.5;

/// Minimum number of shared file names to avoid false positives on tiny directories.
const MIN_SHARED_FILES: usize = 2;

pub(super) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    detect_shadow_modules(fingerprints)
}

/// Directory-level metadata aggregated from file fingerprints.
struct DirInfo {
    /// File names (stem only, no extension) in this directory.
    file_names: HashSet<String>,
    /// All method hashes across all files in this directory.
    method_hashes: HashSet<String>,
    /// All structural hashes across all files (for near-dup detection).
    structural_hashes: HashSet<String>,
    /// Relative directory path.
    dir_path: String,
}

fn detect_shadow_modules(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    // Group fingerprints by parent directory.
    let mut dir_files: HashMap<String, Vec<&FileFingerprint>> = HashMap::new();

    for fp in fingerprints {
        let dir = parent_dir(&fp.relative_path);
        dir_files.entry(dir).or_default().push(fp);
    }

    // Build directory-level metadata.
    let dirs: Vec<DirInfo> = dir_files
        .into_iter()
        .filter(|(_, files)| files.len() >= MIN_SHARED_FILES) // Skip tiny dirs
        .map(|(dir_path, files)| {
            let mut file_names = HashSet::new();
            let mut method_hashes = HashSet::new();
            let mut structural_hashes = HashSet::new();

            for fp in &files {
                if let Some(stem) = file_stem(&fp.relative_path) {
                    file_names.insert(stem);
                }
                for hash in fp.method_hashes.values() {
                    method_hashes.insert(hash.clone());
                }
                for hash in fp.structural_hashes.values() {
                    structural_hashes.insert(hash.clone());
                }
            }

            DirInfo {
                file_names,
                method_hashes,
                structural_hashes,
                dir_path,
            }
        })
        .collect();

    // Compare all directory pairs.
    let mut findings = Vec::new();
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();

    for i in 0..dirs.len() {
        for j in (i + 1)..dirs.len() {
            let a = &dirs[i];
            let b = &dirs[j];

            // Skip if one directory is a subdirectory of the other.
            if a.dir_path.starts_with(&format!("{}/", b.dir_path))
                || b.dir_path.starts_with(&format!("{}/", a.dir_path))
            {
                continue;
            }

            // Skip test directory pairs — test dirs legitimately mirror source dirs.
            if is_test_dir(&a.dir_path) || is_test_dir(&b.dir_path) {
                continue;
            }

            // File name overlap.
            let shared_names: HashSet<_> =
                a.file_names.intersection(&b.file_names).cloned().collect();
            if shared_names.len() < MIN_SHARED_FILES {
                continue;
            }

            let smaller_dir_size = a.file_names.len().min(b.file_names.len());
            let name_overlap = shared_names.len() as f64 / smaller_dir_size as f64;
            if name_overlap < MIN_NAME_OVERLAP {
                continue;
            }

            // Content overlap — check method hash similarity.
            let content_overlap = if a.method_hashes.is_empty() && b.method_hashes.is_empty() {
                // No methods — fall back to structural hash comparison.
                if a.structural_hashes.is_empty() || b.structural_hashes.is_empty() {
                    0.0
                } else {
                    let shared = a
                        .structural_hashes
                        .intersection(&b.structural_hashes)
                        .count();
                    let smaller = a.structural_hashes.len().min(b.structural_hashes.len());
                    shared as f64 / smaller as f64
                }
            } else {
                let shared = a.method_hashes.intersection(&b.method_hashes).count();
                let smaller = a.method_hashes.len().min(b.method_hashes.len());
                if smaller == 0 {
                    0.0
                } else {
                    shared as f64 / smaller as f64
                }
            };

            if content_overlap < MIN_CONTENT_OVERLAP {
                continue;
            }

            // Deduplicate — only report each pair once, with the canonical (shorter path) first.
            let (first, second) = if a.dir_path < b.dir_path {
                (&a.dir_path, &b.dir_path)
            } else {
                (&b.dir_path, &a.dir_path)
            };

            let pair_key = (first.clone(), second.clone());
            if seen_pairs.contains(&pair_key) {
                continue;
            }
            seen_pairs.insert(pair_key);

            let shared_list: Vec<&str> = shared_names.iter().map(|s| s.as_str()).collect();
            let name_pct = (name_overlap * 100.0).round() as u32;
            let content_pct = (content_overlap * 100.0).round() as u32;

            // Emit finding for both directories.
            findings.push(Finding {
                convention: "shadow_module".to_string(),
                severity: Severity::Warning,
                file: first.to_string(),
                description: format!(
                    "Shadow module: `{}` and `{}` share {} files ({name_pct}% name overlap, \
                     {content_pct}% content similarity) — shared: {}",
                    first,
                    second,
                    shared_names.len(),
                    shared_list.join(", ")
                ),
                suggestion: format!(
                    "Consolidate `{}` and `{}` into a single module. \
                     One is likely a copy-paste that was never cleaned up.",
                    first, second
                ),
                kind: AuditFinding::ShadowModule,
            });

            findings.push(Finding {
                convention: "shadow_module".to_string(),
                severity: Severity::Warning,
                file: second.to_string(),
                description: format!(
                    "Shadow module: `{}` and `{}` share {} files ({name_pct}% name overlap, \
                     {content_pct}% content similarity) — shared: {}",
                    second,
                    first,
                    shared_names.len(),
                    shared_list.join(", ")
                ),
                suggestion: format!(
                    "Consolidate into `{}` — this directory appears to be the shadow copy.",
                    first
                ),
                kind: AuditFinding::ShadowModule,
            });
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

/// Extract the parent directory from a relative path.
fn parent_dir(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    match normalized.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => ".".to_string(),
    }
}

/// Extract the file stem (name without extension) from a path.
fn file_stem(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let file_name = normalized
        .rsplit_once('/')
        .map(|(_, f)| f)
        .unwrap_or(&normalized);
    file_name.rsplit_once('.').map(|(stem, _)| stem.to_string())
}

/// Check if a directory path looks like a test directory.
fn is_test_dir(dir: &str) -> bool {
    let lower = dir.to_lowercase();
    lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.ends_with("/tests")
        || lower.ends_with("/test")
        || lower.starts_with("tests/")
        || lower.starts_with("test/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_fp(
        path: &str,
        methods: &[&str],
        method_hashes: &[(&str, &str)],
        structural_hashes: &[(&str, &str)],
    ) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: super::super::conventions::Language::Rust,
            methods: methods.iter().map(|s| s.to_string()).collect(),
            test_methods: vec![],
            registrations: vec![],
            type_name: None,
            type_names: vec![],
            extends: None,
            implements: vec![],
            namespace: None,
            imports: vec![],
            content: String::new(),
            method_hashes: method_hashes
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            structural_hashes: structural_hashes
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            visibility: HashMap::new(),
            properties: vec![],
            hooks: vec![],
            unused_parameters: vec![],
            dead_code_markers: vec![],
            internal_calls: vec![],
            call_sites: vec![],
            public_api: vec![],
            hook_callbacks: vec![],
            trait_impl_methods: vec![],
        }
    }

    #[test]
    fn detects_shadow_modules() {
        let fp1 = make_fp(
            "src/module_a/claims.rs",
            &["extract", "validate"],
            &[("extract", "hash1"), ("validate", "hash2")],
            &[],
        );
        let fp2 = make_fp(
            "src/module_a/verify.rs",
            &["check"],
            &[("check", "hash3")],
            &[],
        );
        let fp3 = make_fp(
            "src/module_b/claims.rs",
            &["extract", "validate"],
            &[("extract", "hash1"), ("validate", "hash2")],
            &[],
        );
        let fp4 = make_fp(
            "src/module_b/verify.rs",
            &["check"],
            &[("check", "hash3")],
            &[],
        );

        let fps: Vec<&FileFingerprint> = vec![&fp1, &fp2, &fp3, &fp4];
        let findings = detect_shadow_modules(&fps);

        assert_eq!(findings.len(), 2, "Should emit findings for both dirs");
        assert!(findings
            .iter()
            .all(|f| f.kind == AuditFinding::ShadowModule));
        assert!(findings
            .iter()
            .any(|f| f.file == "src/module_a" && f.description.contains("src/module_b")));
    }

    #[test]
    fn ignores_test_directories() {
        let fp1 = make_fp("src/module_a/foo.rs", &["run"], &[("run", "hash1")], &[]);
        let fp2 = make_fp("src/module_a/bar.rs", &["exec"], &[("exec", "hash2")], &[]);
        let fp3 = make_fp("tests/module_a/foo.rs", &["run"], &[("run", "hash1")], &[]);
        let fp4 = make_fp(
            "tests/module_a/bar.rs",
            &["exec"],
            &[("exec", "hash2")],
            &[],
        );

        let fps: Vec<&FileFingerprint> = vec![&fp1, &fp2, &fp3, &fp4];
        let findings = detect_shadow_modules(&fps);

        assert!(
            findings.is_empty(),
            "Test dirs should not be flagged as shadows"
        );
    }

    #[test]
    fn ignores_low_overlap() {
        let fp1 = make_fp(
            "src/alpha/common.rs",
            &["shared"],
            &[("shared", "hash1")],
            &[],
        );
        let fp2 = make_fp(
            "src/alpha/unique_a.rs",
            &["only_a"],
            &[("only_a", "hash2")],
            &[],
        );
        let fp3 = make_fp(
            "src/alpha/unique_b.rs",
            &["only_b"],
            &[("only_b", "hash3")],
            &[],
        );
        let fp4 = make_fp(
            "src/beta/common.rs",
            &["shared"],
            &[("shared", "hash1")],
            &[],
        );
        let fp5 = make_fp(
            "src/beta/different_a.rs",
            &["diff_a"],
            &[("diff_a", "hash4")],
            &[],
        );
        let fp6 = make_fp(
            "src/beta/different_b.rs",
            &["diff_b"],
            &[("diff_b", "hash5")],
            &[],
        );

        let fps: Vec<&FileFingerprint> = vec![&fp1, &fp2, &fp3, &fp4, &fp5, &fp6];
        let findings = detect_shadow_modules(&fps);

        // Only 1 shared file name out of 3 = 33% < 50% threshold
        assert!(
            findings.is_empty(),
            "Low overlap should not produce findings"
        );
    }

    #[test]
    fn ignores_subdirectory_relationship() {
        let fp1 = make_fp("src/core/foo.rs", &["run"], &[("run", "hash1")], &[]);
        let fp2 = make_fp("src/core/bar.rs", &["exec"], &[("exec", "hash2")], &[]);
        let fp3 = make_fp("src/core/sub/foo.rs", &["run"], &[("run", "hash1")], &[]);
        let fp4 = make_fp("src/core/sub/bar.rs", &["exec"], &[("exec", "hash2")], &[]);

        let fps: Vec<&FileFingerprint> = vec![&fp1, &fp2, &fp3, &fp4];
        let findings = detect_shadow_modules(&fps);

        assert!(
            findings.is_empty(),
            "Parent/child dirs should not be flagged"
        );
    }

    #[test]
    fn parent_dir_extracts_correctly() {
        assert_eq!(parent_dir("src/core/foo.rs"), "src/core");
        assert_eq!(parent_dir("lib.rs"), ".");
        assert_eq!(parent_dir("src\\win\\bar.rs"), "src/win");
    }

    #[test]
    fn file_stem_extracts_correctly() {
        assert_eq!(file_stem("src/core/foo.rs"), Some("foo".to_string()));
        assert_eq!(file_stem("bar.php"), Some("bar".to_string()));
        assert_eq!(file_stem("noext"), None);
    }
}
