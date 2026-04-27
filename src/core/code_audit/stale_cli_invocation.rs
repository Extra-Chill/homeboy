//! Stale Homeboy CLI invocation detection.
//!
//! Scope is intentionally narrow: scan high-confidence Homeboy command shapes
//! and flag stale static prefixes.

use std::path::Path;

use crate::cli_surface::current_command_surface;
use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};

const STALE_AUDIT_SUBCOMMANDS: &[&str] = &["code", "docs", "structure"];

pub(crate) fn run(root: &Path) -> Vec<Finding> {
    let config = ScanConfig {
        extensions: ExtensionFilter::Only(vec![
            "swift".to_string(),
            "sh".to_string(),
            "bash".to_string(),
            "zsh".to_string(),
        ]),
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

        match path.extension().and_then(|ext| ext.to_str()) {
            Some("swift") => findings.extend(scan_swift_file(&relative, &content)),
            Some("sh" | "bash" | "zsh") => findings.extend(scan_shell_file(&relative, &content)),
            _ => {}
        }
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });
    findings
}

fn scan_shell_file(relative_path: &str, content: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut line_start = 0;

    for line in content.split_inclusive('\n') {
        let line_without_newline = line.trim_end_matches(['\r', '\n']);
        if let Some(strings) = shell_invocation_tokens(line_without_newline) {
            if let Some(finding) =
                stale_invocation_finding(relative_path, content, line_start, &strings)
            {
                findings.push(finding);
            }
        }
        line_start += line.len();
    }

    findings
}

fn shell_invocation_tokens(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') || starts_with_output_command(trimmed) {
        return None;
    }

    if let Some(rest) = strip_homeboy_prefix(trimmed) {
        return static_shell_words(rest, 2);
    }

    if let Some(rest) = command_substitution_homeboy_prefix(trimmed) {
        return static_shell_words(rest, 2);
    }

    if let Some(rest) = quoted_assignment_homeboy_prefix(trimmed) {
        return static_shell_words(rest, 2);
    }

    None
}

fn starts_with_output_command(line: &str) -> bool {
    matches!(first_shell_word(line).as_deref(), Some("echo" | "printf"))
}

fn command_substitution_homeboy_prefix(line: &str) -> Option<&str> {
    if let Some(start) = line.find("$(") {
        let inner = line[start + 2..].trim_start();
        if let Some(rest) = strip_homeboy_prefix(inner) {
            return Some(rest);
        }
    }

    if let Some(start) = line.find('`') {
        let inner = line[start + 1..].trim_start();
        if let Some(rest) = strip_homeboy_prefix(inner) {
            return Some(rest);
        }
    }

    None
}

fn quoted_assignment_homeboy_prefix(line: &str) -> Option<&str> {
    let equals = line.find('=')?;
    let lhs = line[..equals].trim();
    if lhs.is_empty()
        || lhs
            .chars()
            .any(|ch| !(ch == '_' || ch.is_ascii_alphanumeric()))
    {
        return None;
    }

    let rhs = line[equals + 1..].trim_start();
    let quote = rhs.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }

    let inner = rhs[quote.len_utf8()..].trim_start();
    strip_homeboy_prefix(inner)
}

fn strip_homeboy_prefix(input: &str) -> Option<&str> {
    let rest = input.strip_prefix("homeboy")?;
    match rest.chars().next() {
        None => Some(rest),
        Some(ch) if ch.is_whitespace() => Some(rest),
        _ => None,
    }
}

fn first_shell_word(line: &str) -> Option<String> {
    static_shell_words(line, 1)?.into_iter().next()
}

fn static_shell_words(input: &str, limit: usize) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut rest = input.trim_start();

    while !rest.is_empty() && words.len() < limit {
        let first = rest.chars().next()?;
        if matches!(first, '$' | '`' | ';' | '|' | '&' | ')' | '(') {
            break;
        }

        let (word, consumed) = if first == '\'' || first == '"' {
            quoted_shell_word(rest, first)?
        } else {
            unquoted_shell_word(rest)
        };

        if word.is_empty() {
            break;
        }

        words.push(word);
        rest = rest[consumed..].trim_start();
    }

    if words.is_empty() {
        None
    } else {
        Some(words)
    }
}

fn quoted_shell_word(input: &str, quote: char) -> Option<(String, usize)> {
    let mut word = String::new();
    let mut escaped = false;
    let mut consumed = quote.len_utf8();

    for ch in input[quote.len_utf8()..].chars() {
        consumed += ch.len_utf8();
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            return Some((word, consumed));
        }
        if ch == '$' || ch == '`' {
            return Some((word, consumed - ch.len_utf8()));
        }
        word.push(ch);
    }

    None
}

fn unquoted_shell_word(input: &str) -> (String, usize) {
    let mut word = String::new();
    let mut consumed = 0;

    for ch in input.chars() {
        if ch.is_whitespace() || matches!(ch, '$' | '`' | ';' | '|' | '&' | ')' | '(') {
            break;
        }
        consumed += ch.len_utf8();
        word.push(ch);
    }

    (word, consumed)
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
    let surface = current_command_surface();

    if !surface.contains_path(&[command]) {
        return Some(Finding {
            convention: "cli_invocation".to_string(),
            severity: Severity::Warning,
            file: relative_path.to_string(),
            description: format!(
                "Stale Homeboy CLI invocation at line {}: top-level command `{}` no longer exists",
                line, command
            ),
            suggestion: "Use a command exposed by the current Homeboy CLI surface; for removed capability probes, model the capability directly or inspect the target command's help output.".to_string(),
            kind: AuditFinding::StaleCliInvocation,
        });
    }

    if let Some(subcommand) = subcommand {
        if !subcommand.starts_with('-')
            && !surface.contains_path(&[command, subcommand])
            && is_known_removed_subcommand(command, subcommand)
        {
            return Some(Finding {
                convention: "cli_invocation".to_string(),
                severity: Severity::Warning,
                file: relative_path.to_string(),
                description: format!(
                    "Stale Homeboy CLI invocation at line {}: `{} {}` is no longer a valid subcommand shape",
                    line, command, subcommand
                ),
                suggestion: "Use the current command shape from Homeboy's CLI surface; for audit slices, use `homeboy audit <component>` with finding filters where needed.".to_string(),
                kind: AuditFinding::StaleCliInvocation,
            });
        }
    }

    None
}

fn is_known_removed_subcommand(command: &str, subcommand: &str) -> bool {
    command == "audit" && STALE_AUDIT_SUBCOMMANDS.contains(&subcommand)
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

    #[test]
    fn detects_stale_shell_homeboy_invocations() {
        let content = r#"
homeboy supports component
RESULT="$(homeboy audit code my-component)"
FULL_CMD="homeboy audit docs ${COMP_ID}"
BACKTICK=`homeboy audit structure my-component`
"#;

        let findings = scan_shell_file("scripts/run.sh", content);
        let descriptions: Vec<&str> = findings.iter().map(|f| f.description.as_str()).collect();

        assert_eq!(findings.len(), 4);
        assert!(descriptions.iter().any(|d| d.contains("`supports`")));
        assert!(descriptions.iter().any(|d| d.contains("audit code")));
        assert!(descriptions.iter().any(|d| d.contains("audit docs")));
        assert!(descriptions.iter().any(|d| d.contains("audit structure")));
        assert!(findings
            .iter()
            .all(|f| f.kind == AuditFinding::StaleCliInvocation));
    }

    #[test]
    fn ignores_dynamic_and_prose_shell_homeboy_mentions() {
        let content = r#"
echo "Run: homeboy supports component"
printf 'Run: homeboy audit docs component\n'
# homeboy audit docs component
FULL_CMD="homeboy ${CMD}"
homeboy audit "${COMP_ID}" --baseline --path "${WORKSPACE}"
some_homeboy audit code
"#;

        let findings = scan_shell_file("scripts/run.sh", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn run_scans_shell_files_for_stale_invocations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("run.sh");
        std::fs::write(file, "homeboy supports component\n").expect("write fixture");

        let findings = run(dir.path());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "run.sh");
    }
}
