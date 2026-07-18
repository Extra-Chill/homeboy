use homeboy_engine_primitives::language::Language;
use regex::Regex;

// Grammar-driven signature extraction (`MethodSignature`, `extract_signatures*`)
// lives in `code_audit::signatures`, next to the fingerprinting grammar it
// depends on. Refactor's convention-fix codegen consumes it from there, which
// keeps the dependency pointing refactor -> code_audit (no cycle).
pub(crate) use homeboy_code_audit::signature_extraction::{
    extract_signatures_from_items, MethodSignature,
};

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
    items: &'a [homeboy_extension::ParsedItem],
    requested_name: &str,
) -> Option<&'a homeboy_extension::ParsedItem> {
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
) -> Option<Vec<homeboy_extension::ParsedItem>> {
    if let Some(grammar) = homeboy_code_audit::core_fingerprint::load_grammar_for_ext(file_ext) {
        let items = homeboy_extension::grammar_items::parse_items(content, &grammar);
        if !items.is_empty() {
            return Some(
                items
                    .into_iter()
                    .map(homeboy_extension::ParsedItem::from)
                    .collect(),
            );
        }
    }

    let manifest = homeboy_extension::find_extension_for_file_ext(file_ext, "refactor")?;
    let parse_cmd = serde_json::json!({
        "command": "parse_items",
        "file_path": file_path,
        "content": content,
        "items": [],
    });

    homeboy_extension::run_refactor_script(&manifest, &parse_cmd)
        .and_then(|value| value.get("items").cloned())
        .and_then(|value| serde_json::from_value(value).ok())
}
