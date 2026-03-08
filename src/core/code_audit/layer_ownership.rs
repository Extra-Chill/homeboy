//! Layer ownership rules for architecture-level audit constraints.
//!
//! Rules are optional and loaded from either:
//! - `.homeboy/audit-rules.json`
//! - `homeboy.json` under `audit_rules`

use std::path::Path;

use glob_match::glob_match;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AuditRulesConfig {
    #[serde(default)]
    pub layer_rules: Vec<LayerRule>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LayerRule {
    pub name: String,
    pub forbid: LayerForbid,
    #[serde(default)]
    pub allow: Option<LayerAllow>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LayerForbid {
    pub glob: String,
    #[serde(default)]
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LayerAllow {
    pub glob: String,
}

pub(super) fn run(root: &Path) -> Vec<Finding> {
    analyze_layer_ownership(root)
}

fn analyze_layer_ownership(root: &Path) -> Vec<Finding> {
    let Some(config) = load_rules_config(root) else {
        return Vec::new();
    };

    let files = match walk_candidate_files(root) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut findings = Vec::new();

    for file in files {
        let relative = match file.strip_prefix(root) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let normalized = relative.replace('\\', "/");

        for rule in &config.layer_rules {
            if !glob_match(&rule.forbid.glob, &normalized) {
                continue;
            }

            if let Some(allow) = &rule.allow {
                if glob_match(&allow.glob, &normalized) {
                    continue;
                }
            }

            let Ok(content) = std::fs::read_to_string(&file) else {
                continue;
            };

            for pattern in &rule.forbid.patterns {
                if content.contains(pattern) {
                    findings.push(Finding {
                        convention: "layer_ownership".to_string(),
                        severity: Severity::Warning,
                        file: normalized.clone(),
                        description: format!(
                            "Rule '{}' violated: forbidden pattern '{}' matched",
                            rule.name, pattern
                        ),
                        suggestion: format!(
                            "Move this responsibility to the owning layer for rule '{}'",
                            rule.name
                        ),
                        kind: AuditFinding::LayerOwnershipViolation,
                    });
                }
            }
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn walk_candidate_files(root: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    const SKIP_DIRS: &[&str] = &[
        "node_modules",
        "vendor",
        ".git",
        "build",
        "dist",
        "target",
        ".svn",
        ".hg",
        "cache",
        "tmp",
    ];

    fn recurse(
        dir: &Path,
        skip_dirs: &[&str],
        files: &mut Vec<std::path::PathBuf>,
    ) -> std::io::Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if !skip_dirs.contains(&name) {
                    recurse(&path, skip_dirs, files)?;
                }
            } else {
                files.push(path);
            }
        }

        Ok(())
    }

    let mut files = Vec::new();
    recurse(root, SKIP_DIRS, &mut files)?;
    Ok(files)
}

fn load_rules_config(root: &Path) -> Option<AuditRulesConfig> {
    let rules_path = root.join(".homeboy").join("audit-rules.json");
    if let Ok(content) = std::fs::read_to_string(&rules_path) {
        if let Ok(cfg) = serde_json::from_str::<AuditRulesConfig>(&content) {
            return Some(cfg);
        }
    }

    let homeboy_json = root.join("homeboy.json");
    let content = std::fs::read_to_string(homeboy_json).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let audit_rules = value.get("audit_rules")?.clone();
    serde_json::from_value::<AuditRulesConfig>(audit_rules).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_walk_candidate_files_finds_non_extension_files() {
        let dir = tempfile::tempdir().unwrap();
        let steps_dir = dir.path().join("inc/Core/Steps");
        std::fs::create_dir_all(&steps_dir).unwrap();
        std::fs::write(steps_dir.join("agent_ping.php"), "<?php\n").unwrap();
        std::fs::write(steps_dir.join("README.txt"), "notes\n").unwrap();

        let files = walk_candidate_files(dir.path()).unwrap();
        let names: Vec<String> = files
            .iter()
            .filter_map(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(ToString::to_string)
            })
            .collect();

        assert!(names.contains(&"agent_ping.php".to_string()));
        assert!(names.contains(&"README.txt".to_string()));
    }

    #[test]
    fn test_detects_violation_from_audit_rules_file() {
        let dir = tempfile::tempdir().unwrap();
        let homeboy_dir = dir.path().join(".homeboy");
        let steps_dir = dir.path().join("inc/Core/Steps");
        std::fs::create_dir_all(&homeboy_dir).unwrap();
        std::fs::create_dir_all(&steps_dir).unwrap();

        std::fs::write(
            homeboy_dir.join("audit-rules.json"),
            r#"{
              "layer_rules": [
                {
                  "name": "engine-owns-terminal-status",
                  "forbid": {
                    "glob": "inc/Core/Steps/**/*.php",
                    "patterns": ["JobStatus::", "datamachine_fail_job"]
                  },
                  "allow": {"glob": "inc/Abilities/Engine/**/*.php"}
                }
              ]
            }"#,
        )
        .unwrap();

        std::fs::write(
            steps_dir.join("agent_ping.php"),
            "<?php\n$status = JobStatus::FAILED;\n",
        )
        .unwrap();

        let findings = analyze_layer_ownership(dir.path());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].convention, "layer_ownership");
        assert_eq!(findings[0].kind, AuditFinding::LayerOwnershipViolation);
    }

    #[test]
    fn test_supports_homeboy_json_audit_rules() {
        let dir = tempfile::tempdir().unwrap();
        let steps_dir = dir.path().join("inc/Core/Steps");
        std::fs::create_dir_all(&steps_dir).unwrap();

        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{
              "audit_rules": {
                "layer_rules": [
                  {
                    "name": "engine-owns-terminal-status",
                    "forbid": {
                      "glob": "inc/Core/Steps/**/*.php",
                      "patterns": ["datamachine_fail_job"]
                    }
                  }
                ]
              }
            }"#,
        )
        .unwrap();

        std::fs::write(
            steps_dir.join("agent_ping.php"),
            "<?php\ndatamachine_fail_job($job_id);\n",
        )
        .unwrap();

        let findings = analyze_layer_ownership(dir.path());
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn test_no_config_means_no_findings() {
        let dir = tempfile::tempdir().unwrap();
        let findings = analyze_layer_ownership(dir.path());
        assert!(findings.is_empty());
    }

    #[test]
    fn test_run() {
        let dir = tempfile::tempdir().unwrap();
        let findings = run(dir.path());
        assert!(findings.is_empty());
    }

    #[test]
    fn test_load_rules_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{
              "audit_rules": {
                "layer_rules": [
                  {
                    "name": "example-rule",
                    "forbid": {
                      "glob": "src/**/*.rs",
                      "patterns": ["println!"]
                    }
                  }
                ]
              }
            }"#,
        )
        .unwrap();

        let config = load_rules_config(dir.path()).expect("config should load");
        assert_eq!(config.layer_rules.len(), 1);
        assert_eq!(config.layer_rules[0].name, "example-rule");
    }

    #[test]
    fn test_walk_candidate_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();

        let files = walk_candidate_files(dir.path()).expect("walk should succeed");
        assert!(files.iter().any(|p| p.ends_with("src/lib.rs")));
    }
}
