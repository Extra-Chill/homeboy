use super::AuditFinding;
use super::ThinCommandAdapterConfig;
use crate::core::component::ThinCommandAdapterMarkerGroup;

fn write(root: &std::path::Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn config() -> ThinCommandAdapterConfig {
    ThinCommandAdapterConfig {
        include_path_contains: vec!["src/commands/".to_string()],
        file_extensions: vec!["rs".to_string()],
        ignore_line_prefixes: vec!["//".to_string()],
        ignore_after_line_equals: vec!["#[cfg(test)]".to_string()],
        allow_line_contains: vec!["allow-thin-command-adapter".to_string()],
        orchestration_markers: vec![
            ThinCommandAdapterMarkerGroup {
                label: "process execution".to_string(),
                patterns: vec![r"Command::new\s*\(".to_string()],
                weight: 1,
                exempt_when_line_matches: Vec::new(),
            },
            ThinCommandAdapterMarkerGroup {
                label: "filesystem mutation".to_string(),
                patterns: vec![r"std::fs::(write|remove_file)\s*\(".to_string()],
                weight: 1,
                exempt_when_line_matches: Vec::new(),
            },
        ],
        max_orchestration_weight: 2,
        ..Default::default()
    }
}

#[test]
fn empty_config_is_inert() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/foo.rs",
        "fn run() { Command::new(\"x\"); }\n",
    );
    let findings = super::run(dir.path(), &ThinCommandAdapterConfig::default());
    assert!(findings.is_empty());
}

#[test]
fn thick_command_module_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/thick.rs",
        "fn run() {\n    let mut c = Command::new(\"git\");\n    std::fs::write(\"a\", \"b\").unwrap();\n}\n",
    );
    let findings = super::run(dir.path(), &config());
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].kind, AuditFinding::ThinCommandAdapterViolation);
    assert_eq!(findings[0].convention, "thin_command_adapter");
    assert!(findings[0].description.contains("process execution"));
    assert!(findings[0].description.contains("filesystem mutation"));
}

#[test]
fn thin_module_below_threshold_passes() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/thin.rs",
        "fn run() {\n    let mut c = Command::new(\"git\");\n}\n",
    );
    let findings = super::run(dir.path(), &config());
    assert!(findings.is_empty());
}

#[test]
fn files_outside_command_scope_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/core/service.rs",
        "fn run() {\n    let mut c = Command::new(\"git\");\n    std::fs::write(\"a\", \"b\").unwrap();\n}\n",
    );
    let findings = super::run(dir.path(), &config());
    assert!(findings.is_empty());
}

#[test]
fn excluded_path_is_allowlisted() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/transitional.rs",
        "fn run() {\n    let mut c = Command::new(\"git\");\n    std::fs::write(\"a\", \"b\").unwrap();\n}\n",
    );
    let mut config = config();
    config
        .exclude_path_contains
        .push("src/commands/transitional.rs".to_string());
    let findings = super::run(dir.path(), &config);
    assert!(findings.is_empty());
}

#[test]
fn comment_and_allow_lines_do_not_contribute_weight() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/commented.rs",
        "fn run() {\n    // Command::new(\"git\") is documented here\n    std::fs::write(\"a\", \"b\").unwrap(); // allow-thin-command-adapter\n}\n",
    );
    let findings = super::run(dir.path(), &config());
    assert!(findings.is_empty());
}

#[test]
fn test_module_tail_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/with_tests.rs",
        "fn run() {\n    let mut c = Command::new(\"git\");\n}\n#[cfg(test)]\nmod tests {\n    fn t() { std::fs::write(\"a\", \"b\").unwrap(); }\n}\n",
    );
    let findings = super::run(dir.path(), &config());
    assert!(findings.is_empty());
}

fn name_shaped_config() -> ThinCommandAdapterConfig {
    ThinCommandAdapterConfig {
        include_path_contains: vec!["src/commands/".to_string()],
        file_extensions: vec!["rs".to_string()],
        orchestration_markers: vec![ThinCommandAdapterMarkerGroup {
            label: "dispatch".to_string(),
            patterns: vec![r"dispatch_[A-Za-z0-9_]+\s*\(".to_string()],
            weight: 1,
            exempt_when_line_matches: Vec::new(),
        }],
        max_orchestration_weight: 1,
        ..Default::default()
    }
}

#[test]
fn ignore_line_matches_excludes_fn_definitions() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/def_only.rs",
        "pub fn dispatch_foo() {\n    let x = 1;\n}\n",
    );
    let mut config = name_shaped_config();
    config.ignore_line_matches = vec![r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s".to_string()];
    let findings = super::run(dir.path(), &config);
    assert!(
        findings.is_empty(),
        "fn-definition line should not count as orchestration"
    );
}

#[test]
fn ignore_line_matches_still_flags_real_calls() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/real_call.rs",
        "pub fn run() {\n    dispatch_foo();\n}\n",
    );
    let mut config = name_shaped_config();
    config.ignore_line_matches = vec![r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s".to_string()];
    let findings = super::run(dir.path(), &config);
    assert_eq!(
        findings.len(),
        1,
        "an actual dispatch_foo() call must still be flagged"
    );
    assert!(findings[0].description.contains("dispatch"));
}

#[test]
fn empty_ignore_line_matches_preserves_existing_behavior() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/def_only.rs",
        "pub fn dispatch_foo() {\n    let x = 1;\n}\n",
    );
    // With no ignore_line_matches, the fn-def line trips the marker (legacy
    // behavior) and produces a finding.
    let findings = super::run(dir.path(), &name_shaped_config());
    assert_eq!(findings.len(), 1);
}

#[test]
fn weighted_group_reaches_threshold_alone() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/weighted.rs",
        "fn run() {\n    let mut c = Command::new(\"git\");\n}\n",
    );
    let mut config = config();
    config.orchestration_markers[0].weight = 5;
    let findings = super::run(dir.path(), &config);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].description.contains("process execution"));
}

fn delegation_exempt_config() -> ThinCommandAdapterConfig {
    ThinCommandAdapterConfig {
        include_path_contains: vec!["src/commands/".to_string()],
        file_extensions: vec!["rs".to_string()],
        orchestration_markers: vec![ThinCommandAdapterMarkerGroup {
            label: "dispatch".to_string(),
            patterns: vec![r"dispatch_[A-Za-z0-9_]+\s*\(".to_string()],
            weight: 1,
            exempt_when_line_matches: vec![
                r"[A-Za-z_][A-Za-z0-9_]*::(dispatch|execute|persist)_".to_string()
            ],
        }],
        max_orchestration_weight: 1,
        ..Default::default()
    }
}

#[test]
fn exempt_when_line_matches_skips_module_qualified_delegation() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/delegation.rs",
        "pub fn run() {\n    foo::dispatch_thing();\n}\n",
    );
    let findings = super::run(dir.path(), &delegation_exempt_config());
    assert!(
        findings.is_empty(),
        "module-qualified delegation foo::dispatch_thing() must be exempt, not flagged"
    );
}

#[test]
fn exempt_when_line_matches_still_flags_local_call() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/local_call.rs",
        "pub fn run() {\n    dispatch_thing();\n}\n",
    );
    let findings = super::run(dir.path(), &delegation_exempt_config());
    assert_eq!(
        findings.len(),
        1,
        "a bare local dispatch_thing() call must still be flagged"
    );
    assert!(findings[0].description.contains("dispatch"));
}

#[test]
fn concrete_group_with_empty_exempt_always_counts() {
    let dir = tempfile::tempdir().unwrap();
    // A concrete marker line that also contains '::' must still count even
    // though the line is module-qualified — empty exempt means no suppression.
    write(
        dir.path(),
        "src/commands/concrete.rs",
        "pub fn run() {\n    std::fs::write(\"a\", \"b\").unwrap();\n}\n",
    );
    let config = ThinCommandAdapterConfig {
        include_path_contains: vec!["src/commands/".to_string()],
        file_extensions: vec!["rs".to_string()],
        orchestration_markers: vec![ThinCommandAdapterMarkerGroup {
            label: "filesystem mutation".to_string(),
            patterns: vec![r"std::fs::(write|remove_file)\s*\(".to_string()],
            weight: 1,
            exempt_when_line_matches: Vec::new(),
        }],
        max_orchestration_weight: 1,
        ..Default::default()
    };
    let findings = super::run(dir.path(), &config);
    assert_eq!(
        findings.len(),
        1,
        "concrete marker std::fs::write must count despite containing '::'"
    );
    assert!(findings[0].description.contains("filesystem mutation"));
}

#[test]
fn empty_exempt_when_line_matches_preserves_existing_behavior() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/commands/legacy.rs",
        "pub fn run() {\n    foo::dispatch_thing();\n}\n",
    );
    // With no exempt configured, the legacy behavior flags the delegation line.
    let findings = super::run(dir.path(), &name_shaped_config());
    assert_eq!(findings.len(), 1);
}
