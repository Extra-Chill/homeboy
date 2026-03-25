//! grammar — extracted from test.rs.

use std::path::{Path, PathBuf};
use crate::extension::grammar;
use std::collections::HashSet;
use regex::Regex;
use serde::Serialize;
use crate::error::{Error, Result};
use super::ExtractedMethod;
use super::ExtractedClass;


pub fn extract_with_grammar(content: &str, grammar_def: &grammar::Grammar) -> Vec<ExtractedClass> {
    let symbols = grammar::extract(content, grammar_def);
    let ns = grammar::namespace(&symbols).unwrap_or_default();

    let type_symbols: Vec<_> = symbols
        .iter()
        .filter(|symbol| {
            symbol.concept == "class"
                || symbol.concept == "struct"
                || symbol.concept == "trait"
                || symbol.concept == "interface"
                || symbol.concept == "type"
        })
        .collect();

    let method_symbols: Vec<_> = symbols
        .iter()
        .filter(|symbol| {
            symbol.concept == "method"
                || symbol.concept == "function"
                || symbol.concept == "free_function"
        })
        .collect();

    let mut classes = Vec::new();

    if !type_symbols.is_empty() {
        for symbol in &type_symbols {
            let name = symbol.name().unwrap_or("").to_string();
            let kind = symbol
                .get("kind")
                .unwrap_or(symbol.concept.as_str())
                .to_string();

            let methods: Vec<ExtractedMethod> = method_symbols
                .iter()
                .filter(|method| {
                    let name = method.name().unwrap_or("");
                    if name.starts_with("__") && name != "__construct" {
                        return false;
                    }
                    if let Some(modifiers) = method.get("modifiers") {
                        if modifiers.contains("private") {
                            return false;
                        }
                    }
                    true
                })
                .map(|method| {
                    let name = method.name().unwrap_or("").to_string();
                    let visibility = if let Some(modifiers) = method.get("modifiers") {
                        if modifiers.contains("private") {
                            "private"
                        } else if modifiers.contains("protected") {
                            "protected"
                        } else {
                            "public"
                        }
                    } else if let Some(vis) = method.visibility() {
                        if vis.contains("pub") {
                            "pub"
                        } else {
                            "private"
                        }
                    } else {
                        "public"
                    };

                    ExtractedMethod {
                        name,
                        visibility: visibility.to_string(),
                        is_static: method
                            .get("modifiers")
                            .is_some_and(|mods| mods.contains("static"))
                            || method
                                .get("params")
                                .is_some_and(|params| !params.contains("self")),
                        line: method.line,
                        params: method.get("params").unwrap_or("").to_string(),
                    }
                })
                .collect();

            classes.push(ExtractedClass {
                name,
                namespace: ns.clone(),
                kind,
                methods,
            });
        }
    } else if !method_symbols.is_empty() {
        let kind = if grammar_def.language.id == "rust" {
            "module"
        } else {
            "procedural"
        };
        let methods: Vec<ExtractedMethod> = method_symbols
            .iter()
            .map(|method| ExtractedMethod {
                name: method.name().unwrap_or("").to_string(),
                visibility: method.visibility().unwrap_or("public").to_string(),
                is_static: true,
                line: method.line,
                params: method.get("params").unwrap_or("").to_string(),
            })
            .collect();

        classes.push(ExtractedClass {
            name: String::new(),
            namespace: ns,
            kind: kind.to_string(),
            methods,
        });
    }

    classes
}

pub fn load_extension_grammar(extension_path: &Path, language: &str) -> Option<grammar::Grammar> {
    let toml_path = extension_path.join("grammar.toml");
    if toml_path.exists() {
        return grammar::load_grammar(&toml_path).ok();
    }

    let json_path = extension_path.join("grammar.json");
    if json_path.exists() {
        return grammar::load_grammar_json(&json_path).ok();
    }

    let lang_toml = extension_path.join(language).join("grammar.toml");
    if lang_toml.exists() {
        return grammar::load_grammar(&lang_toml).ok();
    }

    None
}
