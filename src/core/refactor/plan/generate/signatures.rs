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
}

pub(crate) fn generate_method_stub(sig: &MethodSignature) -> String {
    let body = stub_body(&sig.name, &sig.language);
    match sig.language {
        Language::Php => format!("\n    {} {{\n{}\n    }}\n", sig.signature, body),
        Language::Rust => format!("\n    {} {{\n{}\n    }}\n", sig.signature, body),
        Language::JavaScript | Language::TypeScript => {
            format!("\n    {} {{\n{}\n    }}\n", sig.signature, body)
        }
        Language::Unknown => String::new(),
    }
}

fn stub_body(method_name: &str, language: &Language) -> String {
    match language {
        Language::Php => {
            format!(
                "        throw new \\RuntimeException('Not implemented: {}');",
                method_name
            )
        }
        Language::Rust => format!("        todo!(\"{}\")", method_name),
        Language::JavaScript | Language::TypeScript => {
            format!(
                "        throw new Error('Not implemented: {}');",
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
    }
}

pub(crate) fn parse_items_for_dedup(
    file_ext: &str,
    content: &str,
    file_path: &str,
) -> Option<Vec<crate::extension::ParsedItem>> {
    if let Some(grammar) = crate::code_audit::core_fingerprint::load_grammar_for_ext(file_ext) {
        let items = crate::utils::grammar_items::parse_items(content, &grammar);
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

    let symbols = crate::utils::grammar::extract(content, &grammar);
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

            Some(MethodSignature {
                name,
                signature,
                language: language.clone(),
            })
        })
        .collect()
}

pub(crate) fn extract_signatures(content: &str, language: &Language) -> Vec<MethodSignature> {
    extract_signatures_from_items(content, language)
}
