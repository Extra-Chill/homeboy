//! Configurable detector for public metadata endpoints bypassing resolvers.

use regex::Regex;

use crate::core::component::PublicRegistryExposureConfig;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

const DEFAULT_ROUTE_CONTEXT_LINES: usize = 8;

pub(super) fn run(
    fingerprints: &[&FileFingerprint],
    config: &PublicRegistryExposureConfig,
) -> Vec<Finding> {
    if config.route_markers.is_empty()
        || config.public_access_markers.is_empty()
        || config.raw_getter_patterns.is_empty()
        || config.permission_aware_resolver_patterns.is_empty()
    {
        return Vec::new();
    }

    let getter_patterns = compile_patterns(&config.raw_getter_patterns);
    let resolver_patterns = compile_patterns(&config.permission_aware_resolver_patterns);
    if getter_patterns.is_empty() || resolver_patterns.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for fp in fingerprints {
        if path_matches(&fp.relative_path, &config.allow_path_contains)
            || !contains_any(&fp.content, &config.route_markers)
            || !contains_any(&fp.content, &config.public_access_markers)
        {
            continue;
        }

        let lines: Vec<&str> = fp.content.lines().collect();
        for (line_index, line) in lines.iter().enumerate() {
            if line_matches(line, &config.allow_line_contains) {
                continue;
            }

            let Some(getter) = first_match(line, &getter_patterns) else {
                continue;
            };

            if !getter_is_in_public_route_context(&lines, line_index, config) {
                continue;
            }

            let Some(resolver_location) =
                resolver_available(fp, fingerprints, &resolver_patterns, config)
            else {
                continue;
            };

            findings.push(Finding {
                convention: "public_registry_exposure".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: format!(
                    "Public metadata endpoint calls raw getter `{}` at line {} while permission-aware resolver marker exists in {}",
                    getter,
                    line_index + 1,
                    resolver_location,
                ),
                suggestion: "Route public metadata responses through the permission-aware resolver/helper, require a safer context, or add an explicit audit allowlist for intentional discovery endpoints.".to_string(),
                kind: AuditFinding::PublicRegistryExposure,
            });
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn compile_patterns(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .collect()
}

fn contains_any(content: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| content.contains(needle))
}

fn path_matches(path: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| path.contains(needle))
}

fn line_matches(line: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| line.contains(needle))
}

fn first_match(line: &str, patterns: &[Regex]) -> Option<String> {
    patterns.iter().find_map(|pattern| {
        pattern
            .find(line)
            .map(|matched| matched.as_str().trim().to_string())
    })
}

fn getter_is_in_public_route_context(
    lines: &[&str],
    line_index: usize,
    config: &PublicRegistryExposureConfig,
) -> bool {
    let context_lines = config
        .route_context_lines
        .unwrap_or(DEFAULT_ROUTE_CONTEXT_LINES);
    let start = line_index.saturating_sub(context_lines);
    let end = (line_index + context_lines + 1).min(lines.len());
    let window = &lines[start..end];

    lines_contain_any(window, &config.route_markers)
        && lines_contain_any(window, &config.public_access_markers)
}

fn lines_contain_any(lines: &[&str], needles: &[String]) -> bool {
    lines.iter().any(|line| contains_any(line, needles))
}

fn resolver_available(
    route_fp: &FileFingerprint,
    fingerprints: &[&FileFingerprint],
    resolver_patterns: &[Regex],
    config: &PublicRegistryExposureConfig,
) -> Option<String> {
    if content_matches(&route_fp.content, resolver_patterns) {
        return Some("the same file".to_string());
    }

    fingerprints
        .iter()
        .filter(|candidate| candidate.relative_path != route_fp.relative_path)
        .filter(|candidate| resolver_candidate_allowed(route_fp, candidate, config))
        .find(|candidate| content_matches(&candidate.content, resolver_patterns))
        .map(|candidate| candidate.relative_path.clone())
}

fn resolver_candidate_allowed(
    route_fp: &FileFingerprint,
    candidate: &FileFingerprint,
    config: &PublicRegistryExposureConfig,
) -> bool {
    path_matches(&candidate.relative_path, &config.resolver_path_contains)
        || (config.resolver_same_namespace && same_namespace(route_fp, candidate))
}

fn content_matches(content: &str, patterns: &[Regex]) -> bool {
    patterns.iter().any(|pattern| pattern.is_match(content))
}

fn same_namespace(left: &FileFingerprint, right: &FileFingerprint) -> bool {
    matches!(
        (&left.namespace, &right.namespace),
        (Some(left_ns), Some(right_ns)) if !left_ns.is_empty() && left_ns == right_ns
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::Language;

    fn fp(path: &str, namespace: Option<&str>, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            namespace: namespace.map(|value| value.to_string()),
            content: content.to_string(),
            ..Default::default()
        }
    }

    fn config() -> PublicRegistryExposureConfig {
        PublicRegistryExposureConfig {
            route_markers: vec!["route(".to_string()],
            public_access_markers: vec!["allow_public".to_string()],
            raw_getter_patterns: vec![r"raw_[a-z_]+_registry\(\)".to_string()],
            permission_aware_resolver_patterns: vec![
                r"PermissionAware[A-Za-z_]+Resolver".to_string()
            ],
            route_context_lines: Some(2),
            resolver_path_contains: vec!["src/policy/".to_string()],
            resolver_same_namespace: false,
            allow_path_contains: vec!["public-discovery".to_string()],
            allow_line_contains: vec!["homeboy-audit: allow-public-registry-exposure".to_string()],
        }
    }

    #[test]
    fn stays_inactive_without_configured_markers() {
        let route = fp(
            "src/api/routes.php",
            None,
            "route('/tools', allow_public, fn() => raw_tool_registry());",
        );

        assert!(run(&[&route], &PublicRegistryExposureConfig::default()).is_empty());
    }

    #[test]
    fn test_run() {
        let route = fp(
            "src/api/routes.php",
            Some("Vendor\\Package\\Api"),
            "route('/tools', allow_public, fn() => raw_tool_registry());",
        );
        let resolver = fp(
            "src/policy/tool-resolver.php",
            Some("Vendor\\Package\\Api"),
            "final class PermissionAwareToolResolver {}",
        );

        let findings = run(&[&route, &resolver], &config());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::PublicRegistryExposure);
        assert!(findings[0].description.contains("raw_tool_registry()"));
        assert!(findings[0]
            .description
            .contains("src/policy/tool-resolver.php"));
    }

    #[test]
    fn skips_raw_getter_outside_public_route_window() {
        let route = fp(
            "src/api/routes.php",
            Some("Vendor\\Package\\Api"),
            "route('/tools', allow_public, fn() => safe_registry());\n\n\nfn helper() { return raw_tool_registry(); }",
        );
        let resolver = fp(
            "src/policy/tool-resolver.php",
            Some("Vendor\\Package\\Api"),
            "final class PermissionAwareToolResolver {}",
        );

        assert!(run(&[&route, &resolver], &config()).is_empty());
    }

    #[test]
    fn requires_explicit_external_resolver_scope() {
        let route = fp(
            "src/api/routes.php",
            Some("Vendor\\Package\\Api"),
            "route('/tools', allow_public, fn() => raw_tool_registry());",
        );
        let resolver = fp(
            "src/api/tool-resolver.php",
            Some("Vendor\\Package\\Api"),
            "final class PermissionAwareToolResolver {}",
        );
        let mut strict_config = config();
        strict_config.resolver_path_contains = vec!["src/policy/".to_string()];
        strict_config.resolver_same_namespace = false;

        assert!(run(&[&route, &resolver], &strict_config).is_empty());
    }

    #[test]
    fn same_namespace_resolver_scope_is_configurable() {
        let route = fp(
            "src/api/routes.php",
            Some("Vendor\\Package\\Api"),
            "route('/tools', allow_public, fn() => raw_tool_registry());",
        );
        let resolver = fp(
            "src/api/tool-resolver.php",
            Some("Vendor\\Package\\Api"),
            "final class PermissionAwareToolResolver {}",
        );
        let mut namespace_config = config();
        namespace_config.resolver_path_contains.clear();
        namespace_config.resolver_same_namespace = true;

        let findings = run(&[&route, &resolver], &namespace_config);

        assert_eq!(findings.len(), 1);
        assert!(findings[0]
            .description
            .contains("src/api/tool-resolver.php"));
    }

    #[test]
    fn skips_when_route_is_not_public_or_no_resolver_exists() {
        let private_route = fp(
            "src/api/private.php",
            None,
            "route('/tools', require_user, fn() => raw_tool_registry());",
        );
        let public_route = fp(
            "src/api/public.php",
            None,
            "route('/tools', allow_public, fn() => raw_tool_registry());",
        );

        assert!(run(&[&private_route, &public_route], &config()).is_empty());
    }

    #[test]
    fn skips_explicit_path_and_line_allowlists() {
        let path_allowed = fp(
            "src/api/public-discovery/tools.php",
            None,
            "route('/tools', allow_public, fn() => raw_tool_registry()); class PermissionAwareToolResolver {}",
        );
        let line_allowed = fp(
            "src/api/tools.php",
            None,
            "route('/tools', allow_public, fn() => raw_tool_registry()); // homeboy-audit: allow-public-registry-exposure\nclass PermissionAwareToolResolver {}",
        );

        assert!(run(&[&path_allowed, &line_allowed], &config()).is_empty());
    }
}
