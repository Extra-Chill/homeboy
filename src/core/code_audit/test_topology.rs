//! Test topology audit — extension-driven test placement policy checks.
//!
//! Core remains language-agnostic. Extensions provide topology signals via
//! `scripts.topology`, and this module enforces repository policy using
//! `audit_rules.test_topology` configuration.

use std::path::Path;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use crate::extension::{self, ExtensionManifest};

#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct AuditRulesConfig {
    #[serde(default)]
    pub test_topology: Option<TestTopologyRules>,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct TestTopologyRules {
    #[serde(default)]
    pub enabled: bool,
    /// Canonical test root(s), usually `tests/**`.
    #[serde(default)]
    pub central_test_globs: Vec<String>,
    /// Optional allowlist for artifacts intentionally kept outside central roots.
    #[serde(default)]
    pub scattered_allow: Vec<String>,
    /// Optional allowlist for source files that may contain inline tests.
    #[serde(default)]
    pub inline_allow: Vec<String>,
    /// Severity for topology findings: "warning" (default) or "info".
    #[serde(default)]
    pub severity: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TopologyInput {
    file_path: String,
    content: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct TopologyOutput {
    #[serde(default)]
    artifacts: Vec<TopologyArtifact>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct TopologyArtifact {
    /// Relative path under component root.
    path: String,
    /// "source" | "test" | other extension-defined tags.
    kind: String,
    /// Optional test shape hint (e.g., "inline", "file").
    #[serde(default)]
    shape: Option<String>,
}

pub(super) fn run(root: &Path) -> Vec<Finding> {
    analyze_test_topology(root)
}

fn analyze_test_topology(root: &Path) -> Vec<Finding> {
    let rules = load_rules(root).unwrap_or_default();
    if !rules.enabled {
        return Vec::new();
    }

    let central_test_globs = if rules.central_test_globs.is_empty() {
        vec!["tests/**".to_string()]
    } else {
        rules.central_test_globs.clone()
    };

    let severity = parse_severity(rules.severity.as_deref());
    let mut findings = Vec::new();

    for extension in extension::load_all_extensions().unwrap_or_default() {
        let Some(script_rel) = extension.topology_script() else {
            continue;
        };

        let files = codebase_scan::walk_files(
            root,
            &ScanConfig {
                // Use extra_skip_dirs so build dirs are skipped at all depths
                // (matching prior flat skip-list behavior)
                extra_skip_dirs: vec![
                    "build".into(),
                    "dist".into(),
                    "target".into(),
                    "cache".into(),
                    "tmp".into(),
                ],
                extensions: ExtensionFilter::All,
                ..Default::default()
            },
        );
        for file in files {
            let rel = match file.strip_prefix(root) {
                Ok(p) => p.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            let Ok(content) = std::fs::read_to_string(&file) else {
                continue;
            };

            let input = TopologyInput {
                file_path: rel.clone(),
                content,
            };

            let artifacts = run_topology_script(&extension, script_rel, &input);
            for artifact in artifacts {
                apply_policy(
                    &artifact,
                    &central_test_globs,
                    &rules,
                    &severity,
                    &mut findings,
                );
            }
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
        .dedup_by(|a, b| a.file == b.file && a.kind == b.kind && a.description == b.description);
    findings
}

fn apply_policy(
    artifact: &TopologyArtifact,
    central_test_globs: &[String],
    rules: &TestTopologyRules,
    severity: &Severity,
    findings: &mut Vec<Finding>,
) {
    let path = &artifact.path;
    let in_central_tests = matches_any(path, central_test_globs);

    if artifact.kind == "test" && !in_central_tests && !matches_any(path, &rules.scattered_allow) {
        findings.push(Finding {
            convention: "test_topology".to_string(),
            severity: severity.clone(),
            file: path.clone(),
            description: "Test artifact is outside centralized test directories".to_string(),
            suggestion: "Move test artifact under central_test_globs (default tests/**) or allowlist it in audit_rules.test_topology.scattered_allow".to_string(),
            kind: AuditFinding::ScatteredTestFile,
        });
    }

    if artifact.kind == "source"
        && artifact.shape.as_deref() == Some("inline_test")
        && !matches_any(path, &rules.inline_allow)
    {
        findings.push(Finding {
            convention: "test_topology".to_string(),
            severity: severity.clone(),
            file: path.clone(),
            description: "Source file contains inline tests outside allowlist".to_string(),
            suggestion: "Prefer isolated tests under central_test_globs; if inline tests are intentional, add this file to audit_rules.test_topology.inline_allow".to_string(),
            kind: AuditFinding::InlineTestModule,
        });
    }
}

fn run_topology_script(
    extension: &ExtensionManifest,
    script_rel: &str,
    input: &TopologyInput,
) -> Vec<TopologyArtifact> {
    let Some(extension_path) = extension.extension_path.as_deref() else {
        return Vec::new();
    };
    let script_path = std::path::Path::new(extension_path).join(script_rel);
    if !script_path.exists() {
        return Vec::new();
    }

    let output = std::process::Command::new("sh")
        .arg(script_path.to_string_lossy().as_ref())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let payload = serde_json::to_vec(input).ok()?;
                let _ = stdin.write_all(&payload);
            }
            child.wait_with_output().ok()
        });

    let Some(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<TopologyOutput>(&stdout)
        .map(|o| o.artifacts)
        .unwrap_or_default()
}

fn parse_severity(value: Option<&str>) -> Severity {
    match value.unwrap_or("warning").to_lowercase().as_str() {
        "info" => Severity::Info,
        _ => Severity::Warning,
    }
}

fn matches_any(path: &str, globs: &[String]) -> bool {
    globs.iter().any(|g| glob_match::glob_match(g, path))
}

fn load_rules(root: &Path) -> Option<TestTopologyRules> {
    let homeboy_json = root.join("homeboy.json");
    let content = std::fs::read_to_string(homeboy_json).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let audit_rules = value.get("audit_rules")?.clone();
    let config: AuditRulesConfig = serde_json::from_value(audit_rules).ok()?;
    config.test_topology
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_severity() {
        assert!(matches!(parse_severity(Some("warning")), Severity::Warning));
        assert!(matches!(parse_severity(Some("info")), Severity::Info));
        assert!(matches!(parse_severity(None), Severity::Warning));
    }

    #[test]
    fn test_matches_any() {
        let globs = vec!["tests/**".to_string(), "spec/**".to_string()];
        assert!(matches_any("tests/unit/foo_test.rs", &globs));
        assert!(!matches_any("src/foo.rs", &globs));
    }

    #[test]
    fn test_apply_policy_flags_scattered_test() {
        let artifact = TopologyArtifact {
            path: "src/foo_test.rs".to_string(),
            kind: "test".to_string(),
            shape: Some("file".to_string()),
        };
        let rules = TestTopologyRules {
            enabled: true,
            central_test_globs: vec!["tests/**".to_string()],
            scattered_allow: vec![],
            inline_allow: vec![],
            severity: None,
        };
        let mut findings = Vec::new();
        apply_policy(
            &artifact,
            &rules.central_test_globs,
            &rules,
            &Severity::Warning,
            &mut findings,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::ScatteredTestFile);
    }

    #[test]
    fn test_apply_policy() {
        let artifact = TopologyArtifact {
            path: "src/lib.rs".to_string(),
            kind: "source".to_string(),
            shape: Some("inline_test".to_string()),
        };
        let rules = TestTopologyRules {
            enabled: true,
            central_test_globs: vec!["tests/**".to_string()],
            scattered_allow: vec![],
            inline_allow: vec![],
            severity: None,
        };
        let mut findings = Vec::new();
        apply_policy(
            &artifact,
            &rules.central_test_globs,
            &rules,
            &Severity::Warning,
            &mut findings,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::InlineTestModule);
    }

    #[test]
    fn test_load_rules() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{
                "audit_rules": {
                    "test_topology": {
                        "enabled": true,
                        "central_test_globs": ["tests/**"]
                    }
                }
            }"#,
        )
        .expect("homeboy.json should be written");

        let rules = load_rules(dir.path()).expect("rules should load");
        assert!(rules.enabled);
        assert_eq!(rules.central_test_globs, vec!["tests/**".to_string()]);
    }

    #[test]
    fn test_run_topology_script() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let script_rel = "topology.sh";
        let script_path = dir.path().join(script_rel);
        std::fs::write(
            &script_path,
            r#"#!/bin/sh
cat <<'JSON'
{"artifacts":[{"path":"src/foo_test.rs","kind":"test","shape":"file"}]}
JSON
"#,
        )
        .expect("script should be written");

        // Keep the command invocation test deterministic by stubbing shell execution,
        // not filesystem permissions. `sh -c <path>` works with readable script files.

        let extension = ExtensionManifest {
            id: "test-ext".to_string(),
            name: "Test Extension".to_string(),
            version: "0.1.0".to_string(),
            provides: None,
            scripts: Some(crate::extension::ScriptsConfig {
                topology: Some(script_rel.to_string()),
                ..Default::default()
            }),
            icon: None,
            description: None,
            author: None,
            homepage: None,
            source_url: None,
            deploy: None,
            audit: None,
            executable: None,
            platform: None,
            cli: None,
            build: None,
            lint: None,
            test: None,
            actions: vec![],
            hooks: std::collections::HashMap::new(),
            settings: vec![],
            requires: None,
            extra: std::collections::HashMap::new(),
            extension_path: Some(dir.path().to_string_lossy().to_string()),
        };

        let artifacts = run_topology_script(
            &extension,
            script_rel,
            &TopologyInput {
                file_path: "src/lib.rs".to_string(),
                content: "pub fn x(){}".to_string(),
            },
        );

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].path, "src/foo_test.rs");
        assert_eq!(artifacts[0].kind, "test");
    }

    #[test]
    fn test_analyze_test_topology() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{
                "audit_rules": {
                    "test_topology": {
                        "enabled": true,
                        "central_test_globs": ["tests/**"],
                        "scattered_allow": [],
                        "inline_allow": []
                    }
                }
            }"#,
        )
        .expect("homeboy.json should be written");

        // No installed extension topology scripts in unit-test context;
        // analyzer should still execute and return deterministic empty findings.
        let findings = analyze_test_topology(dir.path());
        assert!(findings.is_empty());
    }

    #[test]
    fn test_run() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let findings = run(dir.path());
        assert!(findings.is_empty());
    }
}
