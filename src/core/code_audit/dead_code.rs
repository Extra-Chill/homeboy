//! Dead code detection — identify unused parameters, unreferenced exports,
//! orphaned internal functions, and dead code suppression markers.
//!
//! Plugs into the audit pipeline as Phase 4e. Uses data from extension
//! fingerprint scripts (unused_parameters, dead_code_markers, internal_calls,
//! public_api) plus cross-file analysis of imports and method references.

use std::collections::{HashMap, HashSet};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use super::walker::is_test_path;
use crate::component::AuditConfig;

/// A cross-file caller record: which files call a function and with how many args.
struct CallerRecord {
    /// Maximum arg_count seen across all call sites for this function.
    max_arg_count: usize,
    /// Number of distinct call sites across all files.
    call_count: usize,
}

/// Build a caller map from call_sites across all fingerprints.
/// Maps function_name → CallerRecord (max arg count + total call count).
fn build_caller_map(
    owned: &[&FileFingerprint],
    reference: &[&FileFingerprint],
) -> HashMap<String, CallerRecord> {
    let mut map: HashMap<String, CallerRecord> = HashMap::new();

    for fp in owned.iter().chain(reference.iter()) {
        for cs in &fp.call_sites {
            let entry = map.entry(cs.target.clone()).or_insert(CallerRecord {
                max_arg_count: 0,
                call_count: 0,
            });
            entry.max_arg_count = entry.max_arg_count.max(cs.arg_count);
            entry.call_count += 1;
        }
    }

    map
}

/// Analyze fingerprints for dead code patterns.
///
/// Performs four checks on `owned` fingerprints:
/// 1. Unused parameters (from extension fingerprint data, with call-site awareness)
/// 2. Dead code markers (from extension fingerprint data)
/// 3. Unreferenced exports (cross-file: public API never imported/called)
/// 4. Orphaned internals (single-file: private function never called internally)
///
/// `reference` fingerprints contribute calls and imports to the cross-reference
/// set but are NOT checked for dead code themselves. This prevents false positives
/// when framework source is included as a reference dependency.
pub(crate) fn analyze_dead_code_with_config(
    owned: &[&FileFingerprint],
    reference: &[&FileFingerprint],
    audit_config: &AuditConfig,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Build a global set of all internal calls and imports across ALL files
    // (owned + reference) for cross-file reference checking.
    let mut all_calls: HashSet<String> = HashSet::new();
    let mut all_imports: HashSet<String> = HashSet::new();

    for fp in owned.iter().chain(reference.iter()) {
        for call in &fp.internal_calls {
            all_calls.insert(call.clone());
        }
        for import in &fp.imports {
            all_imports.insert(import.clone());
        }
    }

    // Build cross-file caller map for parameter analysis (#824).
    let caller_map = build_caller_map(owned, reference);

    // Only check owned fingerprints for dead code — reference fingerprints
    // just provide call/import data for the cross-reference set.
    for fp in owned {
        // Check 1: Unused parameters — with call-site-aware classification (#824)
        for unused in &fp.unused_parameters {
            let (kind, description, suggestion) = classify_unused_param(unused, &caller_map);

            findings.push(Finding {
                convention: "dead_code".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description,
                suggestion,
                kind,
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
                kind: AuditFinding::DeadCodeMarker,
            });
        }

        // Check 3: Unreferenced exports
        // A public function that no other file imports or calls.
        // Skip test files — test methods are invoked by the test runner via
        // reflection/convention, not by direct calls from other source files.
        if !is_test_path(&fp.relative_path) {
            // For languages without finer-grained visibility than "public" (PHP
            // top-level functions, for example), a public function called from
            // anywhere in its own file IS referenced — the "make it private"
            // suggestion doesn't apply, because PHP top-level functions are
            // always globally accessible. Rust, by contrast, supports
            // `pub(crate)` / `pub(super)` narrowing, so a self-only public
            // function IS actionably dead code.
            let language_allows_self_reference = matches!(
                fp.language,
                super::conventions::Language::Php
                    | super::conventions::Language::JavaScript
                    | super::conventions::Language::TypeScript
            );

            for export in &fp.public_api {
                // Skip if this function is registered as a hook/callback target
                // from within this same file. Such functions ARE referenced —
                // just by the framework runtime rather than by direct calls
                // from other source files.
                // (homeboy#1149 — runtime bootstrap files)
                if fp.hook_callbacks.contains(export) {
                    continue;
                }

                // For languages without finer visibility, same-file direct
                // calls count as references. This catches bootstrap patterns
                // where a top-level helper is called from the entry file before
                // continuing.
                if language_allows_self_reference && fp.internal_calls.contains(export) {
                    continue;
                }

                // Check if any OTHER file (owned or reference) references this export.
                let referenced_elsewhere = owned.iter().chain(reference.iter()).any(|other| {
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
                    if is_framework_entry_point(export, fp, audit_config) {
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
                    kind: AuditFinding::UnreferencedExport,
                });
                }
            }
        } // end if !is_test_path

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
            // Skip trait impl methods — they're called via trait dispatch,
            // not direct function calls, so internal_calls won't contain them.
            if fp.trait_impl_methods.contains(method) {
                continue;
            }

            if !fp.internal_calls.contains(method) {
                // Fallback: check if the method name appears as a call in the
                // file content. internal_calls may miss names in the skip list
                // (e.g., "write" is skipped to avoid matching the write! macro,
                // but it could also be a real function name in this file).
                let call_pattern = format!("{}(", method);
                let method_pattern = format!(".{}(", method);
                if fp.content.contains(&call_pattern) || fp.content.contains(&method_pattern) {
                    continue;
                }

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
                    kind: AuditFinding::OrphanedInternal,
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
fn is_framework_entry_point(name: &str, fp: &FileFingerprint, audit_config: &AuditConfig) -> bool {
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

    // PHP/framework-specific: hook callbacks, lifecycle methods
    if matches!(fp.language, super::conventions::Language::Php) {
        if is_runtime_entrypoint_file(fp, audit_config) {
            return true;
        }

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

fn is_runtime_entrypoint_file(fp: &FileFingerprint, audit_config: &AuditConfig) -> bool {
    let extends = fp.extends.as_deref().unwrap_or("");
    audit_config
        .runtime_entrypoint_extends
        .iter()
        .any(|expected| extends.ends_with(expected))
        || audit_config
            .runtime_entrypoint_markers
            .iter()
            .any(|marker| fp.content.contains(marker))
}

// ============================================================================
// Call-site-aware parameter classification (#824)
// ============================================================================

/// Classify an unused parameter using cross-file call site data.
///
/// Three cases:
/// 1. **No callers found** (or no call_sites data) → `UnusedParameter` (legacy behavior)
/// 2. **Callers exist but none pass enough args** → `UnusedParameter` (truly dead, safe to remove)
/// 3. **Callers pass args for this position** → `IgnoredParameter` (received but ignored, likely a bug)
fn classify_unused_param(
    unused: &crate::extension::UnusedParam,
    caller_map: &HashMap<String, CallerRecord>,
) -> (AuditFinding, String, String) {
    let fn_name = &unused.function;
    let param = &unused.param;
    let position = unused.position;

    match caller_map.get(fn_name) {
        None => {
            // No call site data for this function — fall back to legacy behavior.
            // This happens when: the function is never called, or call_sites
            // aren't available yet (e.g., Rust grammar doesn't emit them).
            (
                AuditFinding::UnusedParameter,
                format!(
                    "Unused parameter '{}' in function '{}' (no callers found)",
                    param, fn_name
                ),
                "Remove the parameter or prefix with underscore to indicate intentional disuse"
                    .to_string(),
            )
        }
        Some(record) => {
            // position is 0-indexed. A caller with arg_count=3 passes positions 0,1,2.
            // So a param at position N is "reached" when arg_count > N.
            let callers_reach_position = record.max_arg_count > position;

            if callers_reach_position {
                // Callers ARE passing values for this position — the function
                // receives the value but ignores it. This is worse than unused:
                // it means callers think the function uses this parameter.
                (
                    AuditFinding::IgnoredParameter,
                    format!(
                        "Parameter '{}' in '{}' is received but ignored ({} caller(s) pass {} arg(s), param is at position {})",
                        param, fn_name, record.call_count, record.max_arg_count, position
                    ),
                    "Either use the parameter (likely a bug) or remove it from both the signature and all call sites"
                        .to_string(),
                )
            } else {
                // Callers don't pass enough args to reach this position.
                // Truly dead — safe to remove from the signature (callers
                // won't need updating since they already don't pass it).
                (
                    AuditFinding::UnusedParameter,
                    format!(
                        "Unused parameter '{}' in '{}' (truly dead — {} caller(s) pass at most {} arg(s), param is at position {})",
                        param, fn_name, record.call_count, record.max_arg_count, position
                    ),
                    "Safe to remove — no caller passes a value for this position"
                        .to_string(),
                )
            }
        }
    }
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
            visibility: vis_map,
            internal_calls: internal_calls.into_iter().map(String::from).collect(),
            public_api: public_api.into_iter().map(String::from).collect(),
            ..Default::default()
        }
    }

    fn analyze_dead_code(
        owned: &[&FileFingerprint],
        reference: &[&FileFingerprint],
    ) -> Vec<Finding> {
        analyze_dead_code_with_config(owned, reference, &AuditConfig::default())
    }

    #[test]
    fn unused_parameter_no_callers() {
        let mut fp = make_fingerprint("src/foo.rs", vec!["process"], vec![], vec![], vec![]);
        fp.unused_parameters.push(UnusedParam {
            function: "process".to_string(),
            param: "ctx".to_string(),
            position: 0,
        });

        let findings = analyze_dead_code(&[&fp], &[]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::UnusedParameter);
        assert!(findings[0].description.contains("ctx"));
        assert!(findings[0].description.contains("process"));
    }

    #[test]
    fn unused_parameter_callers_dont_reach_position() {
        // Function has 3 params, param at position 2 is unused.
        // Callers only pass 2 args (positions 0 and 1) — position 2 is truly dead.
        let mut fp = make_fingerprint("src/foo.rs", vec!["process"], vec![], vec![], vec![]);
        fp.unused_parameters.push(UnusedParam {
            function: "process".to_string(),
            param: "opts".to_string(),
            position: 2,
        });

        // Caller in another file passes only 2 args
        let mut caller_fp = make_fingerprint("src/bar.rs", vec![], vec![], vec!["process"], vec![]);
        caller_fp.call_sites.push(crate::extension::CallSite {
            target: "process".to_string(),
            line: 10,
            arg_count: 2,
        });

        let findings = analyze_dead_code(&[&fp], &[&caller_fp]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::UnusedParameter);
        assert!(
            findings[0].description.contains("truly dead"),
            "Should be classified as truly dead: {}",
            findings[0].description
        );
    }

    #[test]
    fn ignored_parameter_callers_pass_value() {
        // Function has 3 params, param at position 1 is unused.
        // Callers pass 3 args — position 1 IS reached. This is IgnoredParameter.
        let mut fp = make_fingerprint("src/foo.rs", vec!["process"], vec![], vec![], vec![]);
        fp.unused_parameters.push(UnusedParam {
            function: "process".to_string(),
            param: "ctx".to_string(),
            position: 1,
        });

        let mut caller_fp = make_fingerprint("src/bar.rs", vec![], vec![], vec!["process"], vec![]);
        caller_fp.call_sites.push(crate::extension::CallSite {
            target: "process".to_string(),
            line: 10,
            arg_count: 3,
        });

        let findings = analyze_dead_code(&[&fp], &[&caller_fp]);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].kind,
            AuditFinding::IgnoredParameter,
            "Should be IgnoredParameter when callers pass values for the position"
        );
        assert!(
            findings[0].description.contains("received but ignored"),
            "Description should mention 'received but ignored': {}",
            findings[0].description
        );
    }

    #[test]
    fn dead_code_marker_produces_info() {
        let mut fp = make_fingerprint("src/foo.rs", vec!["old_func"], vec![], vec![], vec![]);
        fp.dead_code_markers.push(DeadCodeMarker {
            item: "old_func".to_string(),
            line: 42,
            marker_type: "allow_dead_code".to_string(),
        });

        let findings = analyze_dead_code(&[&fp], &[]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::DeadCodeMarker);
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
        let findings = analyze_dead_code(&[&fp1, &fp2], &[]);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::UnreferencedExport)
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

        let findings = analyze_dead_code(&[&fp1, &fp2], &[]);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::UnreferencedExport)
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

        let findings = analyze_dead_code(&[&fp], &[]);
        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::OrphanedInternal)
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

        let findings = analyze_dead_code(&[&fp], &[]);
        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::OrphanedInternal)
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

        let findings = analyze_dead_code(&[&fp], &[]);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::UnreferencedExport)
            .collect();
        assert!(
            unreferenced.is_empty(),
            "Framework entry points should not be flagged"
        );
    }

    #[test]
    fn trait_impl_methods_not_flagged_as_orphaned() {
        let mut fp = make_fingerprint(
            "src/local_files.rs",
            vec!["read", "write", "delete"],
            vec![],
            vec!["read", "delete"], // write not in internal_calls (skip list)
            vec![
                ("read", "private"),
                ("write", "private"),
                ("delete", "private"),
            ],
        );
        fp.trait_impl_methods = vec![
            "read".to_string(),
            "write".to_string(),
            "delete".to_string(),
        ];

        let findings = analyze_dead_code(&[&fp], &[]);
        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::OrphanedInternal)
            .collect();
        assert!(
            orphaned.is_empty(),
            "Trait impl methods should not be flagged as orphaned, got: {:?}",
            orphaned.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
    }

    #[test]
    fn skipped_call_name_found_in_content_not_flagged() {
        let mut fp = make_fingerprint(
            "src/file.rs",
            vec!["run", "write"],
            vec!["run"],
            vec!["run"], // write not in internal_calls (it's in skip list)
            vec![("write", "private")],
        );
        // The file content contains a direct call to write()
        fp.content = "fn run() { let result = write(&id, &path); }".to_string();

        let findings = analyze_dead_code(&[&fp], &[]);
        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::OrphanedInternal)
            .collect();
        assert!(
            orphaned.is_empty(),
            "Function called in content should not be flagged, got: {:?}",
            orphaned.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
    }

    #[test]
    fn reference_fingerprint_suppresses_unreferenced_export() {
        // Plugin exports "on_save" — nobody in the plugin calls it.
        let plugin_fp = make_fingerprint(
            "inc/handler.php",
            vec!["on_save"],
            vec!["on_save"],
            vec![],
            vec![],
        );

        // Without references: flagged as unreferenced
        let findings = analyze_dead_code(&[&plugin_fp], &[]);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::UnreferencedExport)
            .collect();
        assert_eq!(
            unreferenced.len(),
            1,
            "Should be flagged without references"
        );

        // Framework calls "on_save" via a hook
        let framework_fp = make_fingerprint(
            "wp-includes/plugin.php",
            vec!["do_action"],
            vec!["do_action"],
            vec!["on_save"], // framework calls the plugin's function
            vec![],
        );

        // With references: NOT flagged because framework calls it
        let findings = analyze_dead_code(&[&plugin_fp], &[&framework_fp]);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::UnreferencedExport)
            .collect();
        assert!(
            unreferenced.is_empty(),
            "Should not be flagged when referenced by framework, got: {:?}",
            unreferenced
                .iter()
                .map(|f| &f.description)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn reference_fingerprints_not_checked_for_dead_code() {
        // Framework has an export that nobody calls — should NOT be flagged
        // because framework fingerprints are reference-only.
        let framework_fp = make_fingerprint(
            "vendor/framework/internal.php",
            vec!["internal_helper"],
            vec!["internal_helper"],
            vec![],
            vec![],
        );

        let findings = analyze_dead_code(&[], &[&framework_fp]);
        assert!(
            findings.is_empty(),
            "Reference fingerprints should not produce findings, got: {:?}",
            findings.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
    }

    #[test]
    fn php_hook_callback_suppresses_unreferenced_export() {
        // Runtime-callback pattern: function is defined and registered as a
        // callback in the same file. The dispatcher invokes it, but no other
        // file has a direct call. This is NOT dead code.
        let mut fp = make_fingerprint(
            "plugin.php",
            vec!["activate_plugin"],
            vec!["activate_plugin"],
            vec![], // no self-calls
            vec![],
        );
        fp.language = Language::Php;
        fp.hook_callbacks = vec!["activate_plugin".to_string()];

        let findings = analyze_dead_code(&[&fp], &[]);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::UnreferencedExport)
            .collect();
        assert!(
            unreferenced.is_empty(),
            "Hook-callback registrations should suppress unreferenced_export, got: {:?}",
            unreferenced
                .iter()
                .map(|f| &f.description)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn php_same_file_internal_call_suppresses_unreferenced_export() {
        // Top-level bootstrap pattern: the file calls its own helper functions
        // directly. PHP has no finer visibility than "public" for top-level
        // functions, so "make it private" is not actionable advice — the
        // reference IS the justification for export.
        let mut fp = make_fingerprint(
            "plugin.php",
            vec!["check_runtime_requirements"],
            vec!["check_runtime_requirements"],
            // Same-file call from bootstrap guard.
            vec!["check_runtime_requirements"],
            vec![],
        );
        fp.language = Language::Php;

        let findings = analyze_dead_code(&[&fp], &[]);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::UnreferencedExport)
            .collect();
        assert!(
            unreferenced.is_empty(),
            "PHP same-file internal calls should suppress unreferenced_export, got: {:?}",
            unreferenced
                .iter()
                .map(|f| &f.description)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn rust_same_file_internal_call_still_flags_unreferenced_export() {
        // Rust has pub(crate) / pub(super) for narrowing visibility. A
        // public function only called from its own file IS actionable dead
        // code: the suggestion to narrow visibility is concrete and useful.
        // This test guards against overcorrection from the PHP fix above.
        let fp = make_fingerprint(
            "src/foo.rs",
            vec!["compute"],
            vec!["compute"],
            vec!["compute"], // same-file call
            vec![],
        );
        // language defaults to Rust in make_fingerprint

        let findings = analyze_dead_code(&[&fp], &[]);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::UnreferencedExport)
            .collect();
        assert_eq!(
            unreferenced.len(),
            1,
            "Rust public functions called only from their own file should still be flagged for visibility narrowing"
        );
    }

    #[test]
    fn configured_runtime_dispatched_methods_are_entry_points() {
        let mut fp = make_fingerprint(
            "src/Commands/EmailCommand.php",
            vec!["test_connection"],
            vec!["test_connection"],
            vec![],
            vec![],
        );
        fp.language = Language::Php;
        fp.extends = Some("RuntimeCommand".to_string());
        fp.content = r#"<?php
class EmailCommand extends RuntimeCommand {
    /**
     * Test the IMAP connection.
     *
     * @runtime-entrypoint test-connection
     */
    public function test_connection( array $args, array $assoc_args ): void {}
}
"#
        .to_string();

        let config = AuditConfig {
            runtime_entrypoint_extends: vec!["RuntimeCommand".to_string()],
            runtime_entrypoint_markers: vec!["@runtime-entrypoint".to_string()],
            ..Default::default()
        };

        let findings = analyze_dead_code_with_config(&[&fp], &[], &config);
        let unreferenced: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::UnreferencedExport)
            .collect();
        assert!(
            unreferenced.is_empty(),
            "configured runtime-dispatched methods are invoked by the runtime"
        );
    }
}
