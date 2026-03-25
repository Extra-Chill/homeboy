use crate::code_audit::conventions::Language;
use regex::Regex;

/// Full method signature extracted from a conforming file.
#[derive(Debug, Clone)]
pub(crate) struct MethodSignature {
    /// Method name.
    pub(crate) name: String,
    /// Full signature line (e.g., "public function execute(array $config): array").
    pub(crate) signature: String,
    /// The language this was extracted from.
    #[allow(dead_code)]
    pub(crate) language: Language,
    /// Full method body (between braces), extracted from the conforming file.
    /// None if the body couldn't be extracted.
    pub(crate) body: Option<String>,
}

pub(crate) fn generate_method_stub(sig: &MethodSignature) -> String {
    // Use the real body from a conforming peer when available.
    // Only fall back to a placeholder when no body could be extracted.
    let body = if let Some(ref real_body) = sig.body {
        real_body.clone()
    } else {
        fallback_body(&sig.name, &sig.language)
    };

    // Strip trailing `{` from signature — we add our own.
    let clean_sig = sig.signature.trim_end().trim_end_matches('{').trim_end();

    match sig.language {
        Language::Php => format!("\n    {} {{\n{}\n    }}\n", clean_sig, body),
        Language::Rust => format!("\n    {} {{\n{}\n    }}\n", clean_sig, body),
        Language::JavaScript | Language::TypeScript => {
            format!("\n    {} {{\n{}\n    }}\n", clean_sig, body)
        }
        Language::Unknown => String::new(),
    }
}

/// Last-resort fallback body when no conforming peer could provide one.
/// Produces a clear marker that the method needs implementation.
fn fallback_body(method_name: &str, language: &Language) -> String {
    match language {
        Language::Php => {
            format!(
                "        // TODO: Implement {} — see conforming peers for reference.",
                method_name
            )
        }
        Language::Rust => format!("        todo!(\"{}\")", method_name),
        Language::JavaScript | Language::TypeScript => {
            format!(
                "        // TODO: Implement {} — see conforming peers for reference.",
                method_name
            )
        }
        Language::Unknown => String::new(),
    }
}

pub(crate) fn primary_type_name_from_declaration(
    line: &str,
    language: &Language,
) -> Option<String> {
    let trimmed = line.trim();
    match language {
        Language::Php | Language::TypeScript => Regex::new(r"\b(?:class|interface|trait)\s+(\w+)")
            .ok()?
            .captures(trimmed)
            .map(|cap| cap[1].to_string()),
        Language::Rust => Regex::new(r"\b(?:pub\s+)?(?:struct|enum|trait)\s+(\w+)")
            .ok()?
            .captures(trimmed)
            .map(|cap| cap[1].to_string()),
        Language::JavaScript => Regex::new(r"\bclass\s+(\w+)")
            .ok()?
            .captures(trimmed)
            .map(|cap| cap[1].to_string()),
        Language::Unknown => None,
    }
}

fn normalize_item_name(name: &str) -> String {
    name.trim().to_string()
}

pub(crate) fn find_parsed_item_by_name<'a>(
    items: &'a [crate::extension::ParsedItem],
    requested_name: &str,
) -> Option<&'a crate::extension::ParsedItem> {
    if let Some(exact) = items.iter().find(|item| item.name == requested_name) {
        return Some(exact);
    }

    let requested = normalize_item_name(requested_name);
    let mut normalized_matches = items
        .iter()
        .filter(|item| normalize_item_name(&item.name) == requested);

    let first = normalized_matches.next()?;
    if normalized_matches.next().is_some() {
        return None;
    }

    Some(first)
}

pub(crate) fn generate_fallback_signature(
    method_name: &str,
    language: &Language,
) -> MethodSignature {
    let signature = match language {
        Language::Php => format!("public function {}()", method_name),
        Language::Rust => format!("pub fn {}()", method_name),
        Language::JavaScript | Language::TypeScript => format!("{}()", method_name),
        Language::Unknown => format!("{}()", method_name),
    };

    MethodSignature {
        name: method_name.to_string(),
        signature,
        language: language.clone(),
        body: None,
    }
}

pub(crate) fn parse_items_for_dedup(
    file_ext: &str,
    content: &str,
    file_path: &str,
) -> Option<Vec<crate::extension::ParsedItem>> {
    if let Some(grammar) = crate::code_audit::core_fingerprint::load_grammar_for_ext(file_ext) {
        let items = crate::extension::grammar_items::parse_items(content, &grammar);
        if !items.is_empty() {
            return Some(
                items
                    .into_iter()
                    .map(crate::extension::ParsedItem::from)
                    .collect(),
            );
        }
    }

    let manifest = crate::extension::find_extension_for_file_ext(file_ext, "refactor")?;
    let parse_cmd = serde_json::json!({
        "command": "parse_items",
        "file_path": file_path,
        "content": content,
        "items": [],
    });

    crate::extension::run_refactor_script(&manifest, &parse_cmd)
        .and_then(|value| value.get("items").cloned())
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(crate) fn extract_signatures_from_items(
    content: &str,
    language: &Language,
) -> Vec<MethodSignature> {
    let file_ext = match language {
        Language::Php => "php",
        Language::Rust => "rs",
        Language::JavaScript => "js",
        Language::TypeScript => "ts",
        Language::Unknown => return Vec::new(),
    };

    let Some(grammar) = crate::code_audit::core_fingerprint::load_grammar_for_ext(file_ext) else {
        return Vec::new();
    };

    let symbols = crate::extension::grammar::extract(content, &grammar);
    let lines: Vec<&str> = content.lines().collect();

    symbols
        .into_iter()
        .filter(|symbol| {
            matches!(
                symbol.concept.as_str(),
                "function" | "free_function" | "method"
            )
        })
        .filter_map(|symbol| {
            let name = symbol.name()?.to_string();
            let line_idx = symbol.line.checked_sub(1)?;
            let signature = lines
                .get(line_idx)
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .unwrap_or_else(|| name.clone());

            let body = extract_method_body(&lines, line_idx);

            Some(MethodSignature {
                name,
                signature,
                language: language.clone(),
                body,
            })
        })
        .collect()
}

/// Extract the body of a method from source lines, starting from the
/// declaration line. Finds the opening `{` and walks to the matching `}`,
/// returning the lines between them (the body content).
fn extract_method_body(lines: &[&str], start_line: usize) -> Option<String> {
    let mut brace_depth = 0i32;
    let mut found_open = false;
    let mut body_start_line = start_line + 1;

    for i in start_line..lines.len() {
        let line = lines[i];
        for ch in line.chars() {
            if ch == '{' {
                if !found_open {
                    found_open = true;
                    // Body starts on the NEXT line after the opening brace.
                    body_start_line = i + 1;
                }
                brace_depth += 1;
            } else if ch == '}' {
                brace_depth -= 1;
                if found_open && brace_depth == 0 {
                    // Collect body lines (between opening { line and closing } line).
                    if body_start_line > i {
                        return None; // empty body: `{ }`
                    }
                    let body_lines = &lines[body_start_line..i];
                    let body = body_lines.join("\n");
                    if body.trim().is_empty() {
                        return None;
                    }
                    return Some(body);
                }
            }
        }
    }

    None
}

pub(crate) fn extract_signatures(content: &str, language: &Language) -> Vec<MethodSignature> {
    extract_signatures_from_items(content, language)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_primary_type_name_from_declaration_match_language() {

        let _result = primary_type_name_from_declaration();
    }

    #[test]
    fn test_primary_type_name_from_declaration_ok() {

        let _result = primary_type_name_from_declaration();
    }

    #[test]
    fn test_primary_type_name_from_declaration_ok_2() {

        let _result = primary_type_name_from_declaration();
    }

    #[test]
    fn test_generate_fallback_signature_default_path() {

        let _result = generate_fallback_signature();
    }

    #[test]
    fn test_parse_items_for_dedup_if_let_some_grammar_crate_code_audit_core_fingerprint_load_g() {

        let result = parse_items_for_dedup();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(grammar) = crate::code_audit::core_fingerprint::load_grammar_for_ext(file_ext) {{");
    }

    #[test]
    fn test_parse_items_for_dedup_default_path() {

        let result = parse_items_for_dedup();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_extract_signatures_from_items_let_some_grammar_crate_code_audit_core_fingerprint_load_gram() {

        let result = extract_signatures_from_items();
        assert!(!result.is_empty(), "expected non-empty collection for: let Some(grammar) = crate::code_audit::core_fingerprint::load_grammar_for_ext(file_ext) else {{");
    }

    #[test]
    fn test_extract_signatures_from_items_default_path() {

        let result = extract_signatures_from_items();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_extract_signatures_default_path() {

        let result = extract_signatures();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

}
