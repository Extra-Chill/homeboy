//! Stale Homeboy CLI invocation detection.
//!
//! MVP scope is intentionally narrow: scan Swift array literals that look like
//! Homeboy command arrays and flag high-confidence stale command shapes.

use std::path::Path;

use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};

const STALE_AUDIT_SUBCOMMANDS: &[&str] = &["code", "docs", "structure"];

pub(crate) fn run(root: &Path) -> Vec<Finding> {
    let config = ScanConfig {
        extensions: ExtensionFilter::Only(vec!["swift".to_string()]),
        ..Default::default()
    };
    let mut findings = Vec::new();

    for path in codebase_scan::walk_files(root, &config) {
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let relative = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());

        findings.extend(scan_swift_file(&relative, &content));
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });
    findings
}

fn scan_swift_file(relative_path: &str, content: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let bytes = content.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'[' {
            index += 1;
            continue;
        }

        let Some(end) = find_matching_bracket(content, index) else {
            index += 1;
            continue;
        };
        let literal = &content[index..=end];
        let strings = first_string_literals(literal, 2);

        if strings.is_empty() || !looks_like_homeboy_invocation(content, index) {
            index = end + 1;
            continue;
        }

        if let Some(finding) = stale_invocation_finding(relative_path, content, index, &strings) {
            findings.push(finding);
        }

        index = end + 1;
    }

    findings
}

fn stale_invocation_finding(
    relative_path: &str,
    content: &str,
    offset: usize,
    strings: &[String],
) -> Option<Finding> {
    let command = strings.first()?.as_str();
    let subcommand = strings.get(1).map(|s| s.as_str());
    let line = line_number(content, offset);

    if command == "supports" {
        return Some(Finding {
            convention: "cli_invocation".to_string(),
            severity: Severity::Warning,
            file: relative_path.to_string(),
            description: format!(
                "Stale Homeboy CLI invocation at line {}: top-level command `supports` no longer exists",
                line
            ),
            suggestion: "Remove `homeboy supports`; model the capability directly or probe the target command's help output.".to_string(),
            kind: AuditFinding::StaleCliInvocation,
        });
    }

    if command == "audit" {
        if let Some(subcommand) = subcommand {
            if STALE_AUDIT_SUBCOMMANDS.contains(&subcommand) {
                return Some(Finding {
                    convention: "cli_invocation".to_string(),
                    severity: Severity::Warning,
                    file: relative_path.to_string(),
                    description: format!(
                        "Stale Homeboy CLI invocation at line {}: `audit {}` is no longer a valid subcommand shape",
                        line, subcommand
                    ),
                    suggestion: "Use `homeboy audit <component>` with finding filters where needed; audit no longer has `code`, `docs`, or `structure` subcommands.".to_string(),
                    kind: AuditFinding::StaleCliInvocation,
                });
            }
        }
    }

    None
}

fn looks_like_homeboy_invocation(content: &str, start: usize) -> bool {
    let line_start = content[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = content[start..]
        .find('\n')
        .map(|i| start + i)
        .unwrap_or(content.len());
    let line = &content[line_start..line_end];
    if line.contains("execute") || line.contains("var args =") || line.contains("let args =") {
        return true;
    }

    let prefix_start = start.saturating_sub(120);
    let prefix = content[prefix_start..start].trim_end();
    prefix.ends_with("executeCommand(")
        || prefix.ends_with("executeWithStdin(")
        || prefix.ends_with("execute(")
}

fn find_matching_bracket(content: &str, start: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut in_string = false;
    let mut escaped = false;

    for (offset, byte) in bytes[start..].iter().enumerate() {
        let idx = start + offset;
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match *byte {
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match *byte {
            b'"' => in_string = true,
            b']' if idx > start => return Some(idx),
            _ => {}
        }
    }

    None
}

fn first_string_literals(literal: &str, limit: usize) -> Vec<String> {
    let bytes = literal.as_bytes();
    let mut strings = Vec::new();
    let mut index = 0;

    while index < bytes.len() && strings.len() < limit {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }

        index += 1;
        let mut value = String::new();
        let mut escaped = false;
        while index < bytes.len() {
            let ch = literal[index..].chars().next().unwrap_or_default();
            index += ch.len_utf8();

            if escaped {
                value.push(ch);
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                strings.push(value);
                break;
            }
            value.push(ch);
        }
    }

    strings
}

fn line_number(content: &str, offset: usize) -> usize {
    content[..offset].chars().filter(|c| *c == '\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_stale_swift_homeboy_command_arrays() {
        let content = r#"
final class HomeboyCLI {
    func auditCode(componentId: String) async throws {
        var args = ["audit", "code", componentId]
        _ = try await cli.executeCommand(args)
    }

    func auditDocs(componentId: String) async throws {
        var args = ["audit", "docs", componentId]
        _ = try await cli.executeCommand(args)
    }

    func auditStructure(componentId: String) async throws {
        _ = try await cli.executeCommand(
            ["audit", "structure", componentId]
        )
    }

    func supports(command: String) async throws {
        let args = ["supports", command]
        _ = try await cli.executeCommand(args)
    }
}
"#;

        let findings = scan_swift_file("HomeboyCLI.swift", content);
        let descriptions: Vec<&str> = findings.iter().map(|f| f.description.as_str()).collect();

        assert_eq!(findings.len(), 4);
        assert!(descriptions.iter().any(|d| d.contains("audit code")));
        assert!(descriptions.iter().any(|d| d.contains("audit docs")));
        assert!(descriptions.iter().any(|d| d.contains("audit structure")));
        assert!(descriptions.iter().any(|d| d.contains("`supports`")));
        assert!(findings
            .iter()
            .all(|f| f.kind == AuditFinding::StaleCliInvocation));
    }

    #[test]
    fn ignores_valid_and_unrelated_swift_arrays() {
        let content = r#"
final class HomeboyCLI {
    func valid(componentId: String) async throws {
        var args = ["audit", componentId, "--only", "stale_cli_invocation"]
        _ = try await cli.executeCommand(args)
        _ = try await cli.execute(["server", "key", "show", "prod"])
    }

    func unrelated() {
        let labels = ["audit", "code", "docs"]
        let words = ["supports", "anything"]
    }
}
"#;

        let findings = scan_swift_file("HomeboyCLI.swift", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn run_scans_swift_files_even_without_fingerprints() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("HomeboyCLI.swift");
        std::fs::write(
            file,
            r#"
final class HomeboyCLI {
    func supports(command: String) async throws {
        let args = ["supports", command]
        _ = try await cli.executeCommand(args)
    }
}
"#,
        )
        .expect("write fixture");

        let findings = run(dir.path());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "HomeboyCLI.swift");
    }
}
