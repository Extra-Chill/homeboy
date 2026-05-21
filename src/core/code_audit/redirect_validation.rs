//! Configurable detector for request-derived redirect destinations without
//! dominating URL validation.

use regex::Regex;
use std::sync::LazyLock;

use crate::core::component::RedirectValidationConfig;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

static ASSIGNMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:(?:\$|let\s+|const\s+|var\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=|([A-Za-z_][A-Za-z0-9_]*)\s*:=)"#)
        .expect("request assignment regex compiles")
});
#[derive(Debug, Clone)]
struct TrackedRedirectValue {
    variable: String,
    request_name: String,
    source_line: usize,
}

#[derive(Debug, Clone)]
struct ValidationSite {
    line: usize,
    block_path: Vec<usize>,
}

pub(super) fn run(
    fingerprints: &[&FileFingerprint],
    config: &RedirectValidationConfig,
) -> Vec<Finding> {
    if config.request_names.is_empty()
        || (config.request_source_markers.is_empty() && config.request_source_patterns.is_empty())
        || config.redirect_sinks.is_empty()
        || config.validation_markers.is_empty()
    {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for fp in fingerprints {
        if !eligible_file(fp, config) {
            continue;
        }
        findings.extend(scan_file(fp, config));
    }
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn scan_file(fp: &FileFingerprint, config: &RedirectValidationConfig) -> Vec<Finding> {
    let lines = fp.content.lines().collect::<Vec<_>>();
    let mut tracked = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some(value) = tracked_value_from_line(line, index + 1, config) {
            tracked.push(value);
        }
    }
    if tracked.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for value in tracked {
        let validations = validation_sites_for(&lines, &value, config);
        let block_paths = block_paths_for(&lines);
        for (index, line) in lines.iter().enumerate() {
            let line_number = index + 1;
            if redirect_uses_value(line, &value.variable, config)
                && !validation_dominates(line_number, &block_paths[index], &validations)
            {
                findings.push(Finding {
                    convention: "redirect_validation".to_string(),
                    severity: Severity::Warning,
                    file: fp.relative_path.clone(),
                    description: format!(
                        "Request-derived redirect destination `{}` from `{}` is used at line {} without dominating URL validation",
                        value.variable, value.request_name, line_number
                    ),
                    suggestion: format!(
                        "Validate and allowlist `{}` on every control-flow path before passing it to a configured redirect sink.",
                        value.variable
                    ),
                    kind: AuditFinding::RedirectValidation,
                });
            }
        }
    }
    findings
}

fn tracked_value_from_line(
    line: &str,
    line_number: usize,
    config: &RedirectValidationConfig,
) -> Option<TrackedRedirectValue> {
    let request_name = config
        .request_names
        .iter()
        .find(|name| marker_matches(line, std::slice::from_ref(name)))?;
    if !looks_request_derived(line, config) {
        return None;
    }
    let captures = ASSIGNMENT_RE.captures(line)?;
    let variable = captures
        .get(1)
        .or_else(|| captures.get(2))
        .map(|m| m.as_str().trim_start_matches('$').to_string())?;
    Some(TrackedRedirectValue {
        variable,
        request_name: request_name.clone(),
        source_line: line_number,
    })
}

fn validation_sites_for(
    lines: &[&str],
    value: &TrackedRedirectValue,
    config: &RedirectValidationConfig,
) -> Vec<ValidationSite> {
    let mut sites = Vec::new();
    let block_paths = block_paths_for(lines);
    for (index, line) in lines.iter().enumerate() {
        let line_number = index + 1;
        if line_number > value.source_line
            && line_mentions_variable(line, &value.variable)
            && marker_matches(line, &config.validation_markers)
        {
            sites.push(ValidationSite {
                line: line_number,
                block_path: block_paths[index].clone(),
            });
        }
    }
    sites
}

fn validation_dominates(
    line_number: usize,
    block_path: &[usize],
    validations: &[ValidationSite],
) -> bool {
    validations.iter().any(|site| {
        site.line < line_number
            && site.block_path.len() <= block_path.len()
            && block_path.starts_with(&site.block_path)
    })
}

fn eligible_file(fp: &FileFingerprint, config: &RedirectValidationConfig) -> bool {
    if config
        .exclude_path_contains
        .iter()
        .any(|needle| fp.relative_path.contains(needle))
    {
        return false;
    }
    if config.file_extensions.is_empty() {
        return true;
    }
    let Some(extension) = fp.relative_path.rsplit('.').next() else {
        return false;
    };
    config
        .file_extensions
        .iter()
        .any(|allowed| allowed == extension)
}

fn looks_request_derived(line: &str, config: &RedirectValidationConfig) -> bool {
    marker_matches(line, &config.request_source_markers)
        || patterns_match(line, &config.request_source_patterns)
}

fn redirect_uses_value(line: &str, variable: &str, config: &RedirectValidationConfig) -> bool {
    line_mentions_variable(line, variable) && marker_matches(line, &config.redirect_sinks)
}

fn marker_matches(line: &str, markers: &[String]) -> bool {
    markers
        .iter()
        .any(|marker| !marker.is_empty() && line.contains(marker))
}

fn patterns_match(line: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        !pattern.is_empty()
            && Regex::new(pattern)
                .ok()
                .is_some_and(|regex| regex.is_match(line))
    })
}

fn line_mentions_variable(line: &str, variable: &str) -> bool {
    line.contains(&format!("${variable}"))
        || Regex::new(&format!(r"\b{}\b", regex::escape(variable)))
            .ok()
            .is_some_and(|regex| regex.is_match(line))
}

fn block_paths_for(lines: &[&str]) -> Vec<Vec<usize>> {
    let mut paths = Vec::with_capacity(lines.len());
    let mut path = Vec::new();
    let mut next_block_id = 1usize;
    for line in lines {
        let leading_closes = line
            .chars()
            .take_while(|ch| ch.is_whitespace() || *ch == '}')
            .filter(|ch| *ch == '}')
            .count();
        for _ in 0..leading_closes {
            path.pop();
        }

        paths.push(path.clone());

        let opens = line.chars().filter(|ch| *ch == '{').count();
        let closes = line.chars().filter(|ch| *ch == '}').count();
        for _ in 0..opens {
            path.push(next_block_id);
            next_block_id += 1;
        }
        for _ in leading_closes..closes {
            path.pop();
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::Language;

    fn php_fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            content: content.to_string(),
            ..Default::default()
        }
    }

    fn config() -> RedirectValidationConfig {
        RedirectValidationConfig {
            request_names: vec!["'redirect_uri'".to_string(), "'return_to'".to_string()],
            request_source_markers: vec![
                "$_GET[".to_string(),
                "$_REQUEST[".to_string(),
                "$_POST[".to_string(),
            ],
            request_source_patterns: vec![r#"\brequest\.(query|body)\."#.to_string()],
            redirect_sinks: vec!["redirect_to(".to_string(), "Location:".to_string()],
            validation_markers: vec!["allow_redirect_destination".to_string()],
            file_extensions: vec!["php".to_string()],
            exclude_path_contains: Vec::new(),
        }
    }

    #[test]
    fn test_run() {
        let fp = php_fp(
            "src/Auth.php",
            r#"<?php
$redirect_uri = $_GET['redirect_uri'];
redirect_to($redirect_uri);
"#,
        );

        let findings = run(&[&fp], &config());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::RedirectValidation);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn test_tracked_value_from_line() {
        let value = tracked_value_from_line("$return_to = $_REQUEST['return_to'];", 12, &config())
            .expect("request assignment should be tracked");

        assert_eq!(value.variable, "return_to");
        assert_eq!(value.request_name, "'return_to'");
        assert_eq!(value.source_line, 12);
        assert!(tracked_value_from_line("$safe = get_default_url();", 13, &config()).is_none());
    }

    #[test]
    fn test_validation_sites_for() {
        let lines = [
            "$redirect_uri = $_GET['redirect_uri'];",
            "if ($ok) {",
            "    allow_redirect_destination($redirect_uri);",
            "}",
        ];
        let value = TrackedRedirectValue {
            variable: "redirect_uri".to_string(),
            request_name: "redirect_uri".to_string(),
            source_line: 1,
        };

        let sites = validation_sites_for(&lines, &value, &config());

        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].line, 3);
        assert_eq!(sites[0].block_path, vec![1]);
    }

    #[test]
    fn test_validation_dominates() {
        let validations = vec![ValidationSite {
            line: 3,
            block_path: vec![1],
        }];

        assert!(validation_dominates(4, &[1, 2], &validations));
        assert!(!validation_dominates(4, &[2], &validations));
        assert!(!validation_dominates(3, &[1], &validations));
    }

    #[test]
    fn test_eligible_file() {
        let mut config = config();
        config.exclude_path_contains = vec!["vendor/".to_string()];

        assert!(eligible_file(&php_fp("src/Auth.php", ""), &config));
        assert!(!eligible_file(&php_fp("vendor/Auth.php", ""), &config));
        assert!(!eligible_file(&php_fp("src/Auth.rs", ""), &config));
    }

    #[test]
    fn test_looks_request_derived() {
        let config = config();

        assert!(looks_request_derived(
            "$url = $_POST['redirect_uri'];",
            &config
        ));
        assert!(looks_request_derived(
            "const url = request.query.redirect_uri;",
            &config
        ));
        assert!(!looks_request_derived(
            "const url = configuredRedirect;",
            &config
        ));
    }

    #[test]
    fn test_patterns_match() {
        let patterns = vec![r#"\bcontext\.input\."#.to_string(), "(".to_string()];

        assert!(patterns_match(
            "const url = context.input.return_to;",
            &patterns
        ));
        assert!(!patterns_match(
            "const url = context.output.return_to;",
            &patterns
        ));
    }

    #[test]
    fn test_redirect_uses_value() {
        let config = config();

        assert!(redirect_uses_value(
            "redirect_to($redirect_uri);",
            "redirect_uri",
            &config
        ));
        assert!(redirect_uses_value(
            "redirect_to(redirect_uri);",
            "redirect_uri",
            &config
        ));
        assert!(!redirect_uses_value(
            "render($redirect_uri);",
            "redirect_uri",
            &config
        ));
    }

    #[test]
    fn test_marker_matches() {
        assert!(marker_matches(
            "redirect_to($url);",
            &["redirect_to(".to_string()]
        ));
        assert!(!marker_matches("redirect_to($url);", &[String::new()]));
        assert!(!marker_matches(
            "render($url);",
            &["redirect_to(".to_string()]
        ));
    }

    #[test]
    fn test_marker_matches_request_names_only_as_configured_substrings() {
        let names = vec!["['redirect_uri']".to_string(), ".return_to".to_string()];

        assert!(marker_matches("$_GET['redirect_uri']", &names));
        assert!(marker_matches("request.query.return_to", &names));
        assert!(!marker_matches("redirect_uri_backup", &names));
    }

    #[test]
    fn test_line_mentions_variable() {
        assert!(line_mentions_variable(
            "redirect_to($redirect_uri);",
            "redirect_uri"
        ));
        assert!(line_mentions_variable(
            "redirect_to(redirect_uri);",
            "redirect_uri"
        ));
        assert!(!line_mentions_variable(
            "redirect_to(redirect_uri_backup);",
            "redirect_uri"
        ));
    }

    #[test]
    fn test_block_paths_for() {
        let lines = [
            "if ($ok) {",
            "    while ($next) {",
            "        run();",
            "    }",
            "}",
        ];

        assert_eq!(
            block_paths_for(&lines),
            vec![
                Vec::<usize>::new(),
                vec![1],
                vec![1, 2],
                vec![1],
                Vec::new()
            ]
        );
    }

    #[test]
    fn flags_redirect_when_validation_is_conditional_and_not_dominating() {
        let fp = php_fp(
            "src/Auth.php",
            r#"<?php
$redirect_uri = $_GET['redirect_uri'];
if ($agent) {
    allow_redirect_destination($redirect_uri);
}
if (! $agent) {
    redirect_to($redirect_uri);
}
"#,
        );

        let findings = run(&[&fp], &config());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::RedirectValidation);
        assert!(findings[0].description.contains("redirect_uri"));
    }

    #[test]
    fn accepts_validation_that_dominates_redirect_sink() {
        let fp = php_fp(
            "src/Auth.php",
            r#"<?php
$return_to = $_REQUEST['return_to'];
allow_redirect_destination($return_to);
if ($failed) {
    redirect_to($return_to);
}
"#,
        );

        assert!(run(&[&fp], &config()).is_empty());
    }

    #[test]
    fn ignores_request_values_not_used_by_configured_redirect_sinks() {
        let fp = php_fp(
            "src/Auth.php",
            r#"<?php
$redirect_uri = $_GET['redirect_uri'];
render_link($redirect_uri);
"#,
        );

        assert!(run(&[&fp], &config()).is_empty());
    }
}
