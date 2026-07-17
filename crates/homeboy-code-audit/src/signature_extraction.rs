//! Grammar-driven method-signature extraction.
//!
//! This is a source-parsing primitive: given file content and a language, it
//! uses the audit fingerprinting grammar (`core_fingerprint`) to pull out each
//! method's name, declaration line, and body. It lives in `code_audit` next to
//! the grammar engine it depends on. Both the convention detector (which
//! compares method skeletons across conforming files) and the refactor
//! convention-fix generator (which stubs missing methods from a conforming
//! peer) consume it — refactor imports it *down* from here rather than audit
//! reaching *up* into refactor, which previously formed a `code_audit`↔
//! `refactor` cycle.

use homeboy_engine_primitives::language::Language;

/// Full method signature extracted from a conforming file.
#[derive(Debug, Clone)]
pub struct MethodSignature {
    /// Method name.
    pub name: String,
    /// Full signature line (e.g., "public function execute(array $config): array").
    pub signature: String,
    /// The language this was extracted from.
    pub language: Language,
    /// Full method body (between braces), extracted from the conforming file.
    /// None if the body couldn't be extracted.
    pub body: Option<String>,
}

pub fn extract_signatures_from_items(content: &str, language: &Language) -> Vec<MethodSignature> {
    let file_ext = match language {
        Language::Php => "php",
        Language::Rust => "rs",
        Language::JavaScript => "js",
        Language::TypeScript => "ts",
        Language::Unknown => return Vec::new(),
    };

    let Some(grammar) = super::core_fingerprint::load_grammar_for_ext(file_ext) else {
        return Vec::new();
    };

    let symbols = homeboy_engine_primitives::grammar::extract(content, &grammar);
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
