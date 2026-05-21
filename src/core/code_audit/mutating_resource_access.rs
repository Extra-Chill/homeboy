use regex::Regex;

use crate::core::component::MutatingResourceAccessConfig;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use super::source_locations::line_of_offset;

#[derive(Debug)]
struct HandlerBlock<'a> {
    name: String,
    body: &'a str,
    start_offset: usize,
}

pub(super) fn run(
    fingerprints: &[&FileFingerprint],
    config: &MutatingResourceAccessConfig,
) -> Vec<Finding> {
    if config.is_empty()
        || config.handler_registration_markers.is_empty()
        || config.mutating_operation_markers.is_empty()
        || config.resource_identifier_patterns.is_empty()
        || config.mutator_markers.is_empty()
    {
        return Vec::new();
    }

    let resource_patterns = config
        .resource_identifier_patterns
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .collect::<Vec<_>>();

    if resource_patterns.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for fp in fingerprints {
        if !contains_any(&fp.content, &config.handler_registration_markers)
            || !contains_any(&fp.content, &config.mutating_operation_markers)
        {
            continue;
        }

        for block in extract_handler_blocks(&fp.content) {
            if !contains_any(block.body, &config.mutating_operation_markers)
                && !handler_is_registered_mutator(&fp.content, &block.name, config)
            {
                continue;
            }
            if !contains_any_regex(block.body, &resource_patterns)
                || !contains_any(block.body, &config.mutator_markers)
            {
                continue;
            }
            if contains_any(block.body, &config.access_helper_markers)
                || contains_any(block.body, &config.trusted_delegation_markers)
            {
                continue;
            }

            let line = line_of_offset(&fp.content, block.start_offset);
            findings.push(Finding {
                convention: "mutating_resource_access".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: format!(
                    "Mutating handler `{}` touches a configured resource identifier without a configured ownership/access check (line {}).",
                    block.name, line
                ),
                suggestion:
                    "Add a configured access helper call before mutating the resource, or route through a configured trusted delegation marker."
                        .to_string(),
                kind: AuditFinding::MutatingResourceAccess,
            });
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn handler_is_registered_mutator(
    content: &str,
    handler_name: &str,
    config: &MutatingResourceAccessConfig,
) -> bool {
    content.match_indices(handler_name).any(|(offset, _)| {
        let start = offset.saturating_sub(512);
        let end = (offset + handler_name.len() + 512).min(content.len());
        let window = &content[start..end];

        contains_any(window, &config.handler_registration_markers)
            && contains_any(window, &config.mutating_operation_markers)
    })
}

fn extract_handler_blocks(content: &str) -> Vec<HandlerBlock<'_>> {
    let Ok(regex) = Regex::new(
        r"(?m)(?:public|protected|private|static|async|final|abstract|\s)*\b(?:function|fn)\s+([A-Za-z_][A-Za-z0-9_]*)\s*\([^;{}]*\)\s*\{",
    ) else {
        return Vec::new();
    };

    let mut blocks = Vec::new();
    for captures in regex.captures_iter(content) {
        let Some(full_match) = captures.get(0) else {
            continue;
        };
        let Some(name_match) = captures.get(1) else {
            continue;
        };
        let body_start = full_match.end();
        let Some(body_end) = matching_brace_end(content, body_start.saturating_sub(1)) else {
            continue;
        };
        blocks.push(HandlerBlock {
            name: name_match.as_str().to_string(),
            body: &content[body_start..body_end],
            start_offset: full_match.start(),
        });
    }
    blocks
}

fn matching_brace_end(content: &str, open_brace_offset: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    if bytes.get(open_brace_offset).copied() != Some(b'{') {
        return None;
    }

    let mut depth = 0usize;
    for (offset, byte) in bytes.iter().enumerate().skip(open_brace_offset) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn contains_any(content: &str, markers: &[String]) -> bool {
    markers
        .iter()
        .any(|marker| !marker.is_empty() && content.contains(marker))
}

fn contains_any_regex(content: &str, patterns: &[Regex]) -> bool {
    patterns.iter().any(|pattern| pattern.is_match(content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::conventions::Language;

    fn config() -> MutatingResourceAccessConfig {
        MutatingResourceAccessConfig {
            handler_registration_markers: vec!["route(".to_string()],
            mutating_operation_markers: vec!["WRITE".to_string(), "DELETE".to_string()],
            resource_identifier_patterns: vec![r"\b[a-z_]*_id\b".to_string()],
            access_helper_markers: vec!["Access::owns_resource".to_string()],
            trusted_delegation_markers: vec!["CheckedAbility".to_string()],
            mutator_markers: vec!["save_resource(".to_string(), "delete_resource(".to_string()],
        }
    }

    fn fp(content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: "src/handlers.php".to_string(),
            language: Language::Php,
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_run() {
        let fp = fp(r#"
route('/things/(?P<thing_id>\\d+)', ['methods' => 'WRITE', 'callback' => 'update_thing']);
route('/things/(?P<thing_id>\\d+)/delete', ['methods' => 'DELETE', 'callback' => 'delete_thing']);
route('/things/(?P<thing_id>\\d+)/clone', ['methods' => 'WRITE', 'callback' => 'clone_thing']);

public function update_thing($request) {
    $thing_id = $request['thing_id'];
    save_resource($thing_id);
}

public function delete_thing($request) {
    $thing_id = $request['thing_id'];
    Access::owns_resource($thing_id);
    delete_resource($thing_id);
}

public function clone_thing($request) {
    $thing_id = $request['thing_id'];
    CheckedAbility::run($thing_id);
    save_resource($thing_id);
}
"#);

        let findings = run(&[&fp], &config());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::MutatingResourceAccess);
        assert!(findings[0].description.contains("update_thing"));
    }

    #[test]
    fn flags_mutating_registered_handler_without_access_check() {
        let fp = fp(r#"
route('/things/(?P<thing_id>\\d+)', ['methods' => 'WRITE', 'callback' => 'update_thing']);

public function update_thing($request) {
    $thing_id = $request['thing_id'];
    save_resource($thing_id);
}
"#);

        let findings = run(&[&fp], &config());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::MutatingResourceAccess);
        assert!(findings[0].description.contains("update_thing"));
    }

    #[test]
    fn accepts_direct_access_helper_and_trusted_delegation() {
        let fp = fp(r#"
route('/things/(?P<thing_id>\\d+)', ['methods' => 'WRITE', 'callback' => 'update_thing']);
route('/things/(?P<thing_id>\\d+)/clone', ['methods' => 'WRITE', 'callback' => 'clone_thing']);

public function update_thing($request) {
    $thing_id = $request['thing_id'];
    Access::owns_resource($thing_id);
    save_resource($thing_id);
}

public function clone_thing($request) {
    $thing_id = $request['thing_id'];
    CheckedAbility::run($thing_id);
    save_resource($thing_id);
}
"#);

        assert!(run(&[&fp], &config()).is_empty());
    }

    #[test]
    fn does_not_infer_access_helper_from_sibling_handler_call_shape() {
        let mut config = config();
        config.access_helper_markers.clear();
        let fp = fp(r#"
route('/things/(?P<thing_id>\\d+)', ['methods' => 'WRITE', 'callback' => 'update_thing']);
route('/things/(?P<thing_id>\\d+)/rename', ['methods' => 'WRITE', 'callback' => 'rename_thing']);

public function update_thing($request) {
    $thing_id = $request['thing_id'];
    can_edit_resource($thing_id);
    save_resource($thing_id);
}

public function rename_thing($request) {
    $thing_id = $request['thing_id'];
    save_resource($thing_id);
}
"#);

        let findings = run(&[&fp], &config);

        assert_eq!(findings.len(), 2);
        assert!(findings[0].description.contains("rename_thing"));
        assert!(findings[1].description.contains("update_thing"));
    }

    #[test]
    fn accepts_configured_access_helper_marker_only() {
        let mut config = config();
        config.access_helper_markers = vec!["can_edit_resource(".to_string()];
        let fp = fp(r#"
route('/things/(?P<thing_id>\\d+)', ['methods' => 'WRITE', 'callback' => 'update_thing']);
route('/things/(?P<thing_id>\\d+)/rename', ['methods' => 'WRITE', 'callback' => 'rename_thing']);

public function update_thing($request) {
    $thing_id = $request['thing_id'];
    can_edit_resource($thing_id);
    save_resource($thing_id);
}

public function rename_thing($request) {
    $thing_id = $request['thing_id'];
    save_resource($thing_id);
}
"#);

        let findings = run(&[&fp], &config);

        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("rename_thing"));
    }

    #[test]
    fn ignores_unregistered_mutators() {
        let fp = fp(r#"
public function helper($thing_id) {
    save_resource($thing_id);
}
"#);

        assert!(run(&[&fp], &config()).is_empty());
    }

    #[test]
    fn test_handler_is_registered_mutator() {
        let config = config();
        let content = r#"
route('/things/(?P<thing_id>\\d+)', ['methods' => 'WRITE', 'callback' => 'update_thing']);

public function update_thing($request) {
    $thing_id = $request['thing_id'];
    save_resource($thing_id);
}

// Keep the non-mutating registration outside the fixed-size mutating window.
// ................................................................................
// ................................................................................
// ................................................................................
// ................................................................................
// ................................................................................
// ................................................................................
// ................................................................................
// ................................................................................
route('/things/(?P<thing_id>\\d+)', ['methods' => 'READ', 'callback' => 'read_thing']);
"#;

        assert!(handler_is_registered_mutator(
            content,
            "update_thing",
            &config
        ));
        assert!(!handler_is_registered_mutator(
            content,
            "read_thing",
            &config
        ));
        assert!(!handler_is_registered_mutator(
            content,
            "missing_thing",
            &config
        ));
    }

    #[test]
    fn test_extract_handler_blocks() {
        let content = r#"
public function update_thing($request) {
    if ($request['thing_id']) {
        save_resource($request['thing_id']);
    }
}

private static function helper($thing_id) {
    return $thing_id;
}
"#;

        let blocks = extract_handler_blocks(content);

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].name, "update_thing");
        assert!(blocks[0].body.contains("save_resource"));
        assert_eq!(blocks[1].name, "helper");
    }

    #[test]
    fn contains_any_matches_non_empty_markers() {
        assert!(contains_any(
            "Access::owns_resource($thing_id);",
            &["owns_resource".to_string()]
        ));
        assert!(!contains_any(
            "Access::owns_resource($thing_id);",
            &[String::new(), "can_edit_resource".to_string()]
        ));
    }

    #[test]
    fn test_contains_any_regex() {
        let patterns = vec![Regex::new(r"\b[a-z_]*_id\b").unwrap()];

        assert!(contains_any_regex("$thing_id = 1;", &patterns));
        assert!(!contains_any_regex("$thing = 1;", &patterns));
    }
}
