//! Dead code detection — identify unused parameters, unreferenced exports,
//! orphaned internal functions, and dead code suppression markers.
//!
//! Plugs into the audit pipeline as Phase 4e. Uses data from extension
//! fingerprint scripts (unused_parameters, dead_code_markers, internal_calls,
//! public_api) plus cross-file analysis of imports and method references.

use std::collections::HashSet;

use super::conventions::DeviationKind;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

/// Analyze fingerprints for dead code patterns.
///
/// Performs four checks:
/// 1. Unused parameters (from extension fingerprint data)
/// 2. Dead code markers (from extension fingerprint data)
/// 3. Unreferenced exports (cross-file: public API never imported/called)
/// 4. Orphaned internals (single-file: private function never called internally)
pub(crate) fn analyze_dead_code(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Build a global set of all internal calls and imports across all files
    // for cross-file reference checking.
    let mut all_calls: HashSet<String> = HashSet::new();
    let mut all_imports: HashSet<String> = HashSet::new();

    for fp in fingerprints {
        for call in &fp.internal_calls {
            all_calls.insert(call.clone());
        }
        for import in &fp.imports {
            all_imports.insert(import.clone());
        }
    }

    for fp in fingerprints {
        // Check 1: Unused parameters
        for unused in &fp.unused_parameters {
            findings.push(Finding {
                convention: "dead_code".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: format!(
                    "Unused parameter '{}' in function '{}'",
                    unused.param, unused.function
                ),
                suggestion:
                    "Remove the parameter or prefix with underscore to indicate intentional disuse"
                        .to_string(),
                kind: DeviationKind::UnusedParameter,
            });
        }

        // Check 2: Dead code markers
        for marker in &fp.dead_code_markers {
            findings.push(Finding {
                convention: "dead_code".to_string(),
                severity: Severity::Info,
                file: fp.relative_path.clone(),
                description: format!(
                    "Dead code marker on '{}' (line {}, type: {})",
                    marker.item, marker.line, marker.marker_type
                ),
                suggestion:
                    "Remove the dead code instead of suppressing the warning, or document why it must stay"
                        .to_string(),
                kind: DeviationKind::DeadCodeMarker,
            });
        }

        // Check 3: Unreferenced exports
        // A public function that no other file imports or calls.
        for export in &fp.public_api {
            // Check if any OTHER file references this export.
            let referenced_elsewhere = fingerprints.iter().any(|other| {
                // Skip self
                if other.relative_path == fp.relative_path {
                    return false;
                }
                // Check if the other file calls this function
                if other.internal_calls.contains(export) {
                    return true;
                }
                // Check if the other file imports something that matches
                // (import paths may contain the type/module name, not the function name directly)
                let type_name = fp.type_name.as_deref().unwrap_or("");
                other.imports.iter().any(|imp| {
                    // Direct function name match in import
                    imp.contains(export.as_str())
                    // Or imports the type that contains this method
                    || (!type_name.is_empty() && imp.contains(type_name))
                })
            });

            if !referenced_elsewhere {
                // Skip common entry points and framework methods that are called
                // by the runtime, not by other source files.
                if is_framework_entry_point(export, fp) {
                    continue;
                }

                findings.push(Finding {
                    convention: "dead_code".to_string(),
                    severity: Severity::Info,
                    file: fp.relative_path.clone(),
                    description: format!(
                        "Public function '{}' is not referenced by any other file",
                        export
                    ),
                    suggestion:
                        "Consider making it private/pub(crate), removing it, or verifying it's used externally"
                            .to_string(),
                    kind: DeviationKind::UnreferencedExport,
                });
            }
        }

        // Check 4: Orphaned internals
        // Private functions that are never called within the same file.
        let private_methods: Vec<&String> = fp
            .methods
            .iter()
            .filter(|m| {
                fp.visibility
                    .get(*m)
                    .map(|v| v == "private")
                    .unwrap_or(false)
            })
            .collect();

        for method in private_methods {
            if !fp.internal_calls.contains(method) {
                findings.push(Finding {
                    convention: "dead_code".to_string(),
                    severity: Severity::Warning,
                    file: fp.relative_path.clone(),
                    description: format!(
                        "Private function '{}' is never called within this file",
                        method
                    ),
                    suggestion: "Remove the dead function or make it public if used externally"
                        .to_string(),
                    kind: DeviationKind::OrphanedInternal,
                });
            }
        }
    }

    // Sort by file path for deterministic output
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

/// Check if a function name is a framework entry point that's expected to be
/// called by the runtime rather than other source files.
///
/// These are common patterns across languages where functions are invoked by
/// convention/framework rather than explicit calls from other source files.
fn is_framework_entry_point(name: &str, fp: &FileFingerprint) -> bool {
    // Common entry points across all languages
    let universal_entry_points = [
        "main", "new", "default", "from", "try_from", "into", "drop", "clone", "fmt", "display",
        "eq", "hash",
    ];
    if universal_entry_points.contains(&name) {
        return true;
    }

    // Rust-specific: trait implementations are called by the type system
    if matches!(fp.language, super::conventions::Language::Rust) {
        // Methods inside impl blocks for standard traits
        let rust_trait_methods = [
            "serialize",
            "deserialize",
            "from_str",
            "as_ref",
            "deref",
            "index",
            "add",
            "sub",
            "mul",
            "div",
            "neg",
            "not",
            "build",
            "run",
            "execute",
            "augment_args",
            "augment_subcommands",
            "from_arg_matches",
            "update_from_arg_matches",
            "command",
            "value_variants",
        ];
        if rust_trait_methods.contains(&name) {
            return true;
        }
    }

    // PHP/WordPress-specific: hook callbacks, lifecycle methods
    if matches!(fp.language, super::conventions::Language::Php) {
        let php_entry_points = [
            "__construct",
            "__destruct",
            "__get",
            "__set",
            "__call",
            "__callStatic",
            "__toString",
            "__invoke",
            "__clone",
            "__sleep",
            "__wakeup",
            "register",
            "init",
            "activate",
            "deactivate",
            "boot",
            "setup",
            "render",
            "handle",
            "process",
        ];
        if php_entry_points.contains(&name) {
            return true;
        }
    }

    false
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::Language;
    use crate::extension::{DeadCodeMarker, UnusedParam};
    use std::collections::HashMap;

    fn make_fingerprint(
        path: &str,
        methods: Vec<&str>,
        public_api: Vec<&str>,
        internal_calls: Vec<&str>,
        visibility: Vec<(&str, &str)>,
    ) -> FileFingerprint {
        let vis_map: HashMap<String, String> = visibility
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Rust,
            methods: methods.into_iter().map(String::from).collect(),
            registrations: vec![],
            type_name: None,
            extends: None,
            implements: vec![],
            namespace: None,
            imports: vec![],
            content: String::new(),
            method_hashes: HashMap::new(),
            structural_hashes: HashMap::new(),
            visibility: vis_map,
            properties: vec![],
            hooks: vec![],
            unused_parameters: vec![],
            dead_code_markers: vec![],
            internal_calls: internal_calls.into_iter().map(String::from).collect(),
            public_api: public_api.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn unused_parameter_produces_warning() {
        let mut fp = make_fingerprint("src/foo.rs", vec!["process"], vec![], vec![], vec![]);
        fp.unused_parameters.push(UnusedParam {
            function: "process".to_string(),
            param: "ctx".to_string(),
        });

        let findings = analyze_dead_code(&[&fp]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, DeviationKind::UnusedParameter);
        assert!(findings[0].description.contains("ctx"));
        assert!(findings[0].description.contains("process"));
    }

    #[test]
    fn dead_code_marker_produces_info() {
        let mut fp = make_fingerprint("src/foo.rs", vec!["old_func"], vec![], vec![], vec![]);
        fp.dead_code_markers.push(DeadCodeMarker {
            item: "old_func".to_string(),
            line: 42,
            marker_type: "allow_dead_code".to_string(),
        });

        let findings = analyze_dead_code(&[&fp]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, DeviationKind::DeadCodeMarker);
        assert_eq!(findings[0].severity, Severity::Info);
    }

    #[test]
    fn unreferenced_export_detected() {
        let fp1 = make_fingerprint(
            "src/foo.rs",
            vec!["compute"],
            vec!["compute"],
            vec![],
            vec![],
        );
        let fp2 = make_fingerprint(
            "src/bar.rs",
            vec!["transform"],
            vec!["transform"],
            vec![],
            vec![],
        );

        // Neither file calls the other's exports
        let findings = analyze_dead_code(&[&fp1, &fp2]);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == DeviationKind::UnreferencedExport)
            .collect();
        assert_eq!(unreferenced.len(), 2); // compute and transform both unreferenced
    }

    #[test]
    fn referenced_export_not_flagged() {
        let fp1 = make_fingerprint(
            "src/foo.rs",
            vec!["compute"],
            vec!["compute"],
            vec![],
            vec![],
        );
        let fp2 = make_fingerprint(
            "src/bar.rs",
            vec!["transform"],
            vec!["transform"],
            vec!["compute"], // bar calls compute
            vec![],
        );

        let findings = analyze_dead_code(&[&fp1, &fp2]);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == DeviationKind::UnreferencedExport)
            .collect();
        // Only "transform" is unreferenced (nobody calls it), "compute" is called by bar
        assert_eq!(unreferenced.len(), 1);
        assert!(unreferenced[0].description.contains("transform"));
    }

    #[test]
    fn orphaned_private_function_detected() {
        let fp = make_fingerprint(
            "src/foo.rs",
            vec!["public_fn", "dead_helper"],
            vec!["public_fn"],
            vec!["public_fn"], // calls public_fn but not dead_helper
            vec![("dead_helper", "private")],
        );

        let findings = analyze_dead_code(&[&fp]);
        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == DeviationKind::OrphanedInternal)
            .collect();
        assert_eq!(orphaned.len(), 1);
        assert!(orphaned[0].description.contains("dead_helper"));
    }

    #[test]
    fn called_private_function_not_flagged() {
        let fp = make_fingerprint(
            "src/foo.rs",
            vec!["public_fn", "helper"],
            vec!["public_fn"],
            vec!["helper"], // calls helper
            vec![("helper", "private")],
        );

        let findings = analyze_dead_code(&[&fp]);
        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == DeviationKind::OrphanedInternal)
            .collect();
        assert!(orphaned.is_empty());
    }

    #[test]
    fn framework_entry_points_not_flagged() {
        let fp = make_fingerprint(
            "src/foo.rs",
            vec!["main", "new", "default"],
            vec!["main", "new", "default"],
            vec![],
            vec![],
        );

        let findings = analyze_dead_code(&[&fp]);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == DeviationKind::UnreferencedExport)
            .collect();
        assert!(
            unreferenced.is_empty(),
            "Framework entry points should not be flagged"
        );
    }
}
