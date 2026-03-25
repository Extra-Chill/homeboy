//! detect — extracted from contract_extract.rs.

use regex::Regex;
use crate::extension::grammar::{self, ContractGrammar, Grammar, Region};
use super::super::contract::*;


/// Detect the receiver type from the params string.
pub(crate) fn detect_receiver(params_str: &str) -> Option<Receiver> {
    let first = params_str.split(',').next()?.trim();
    if first == "&mut self" {
        Some(Receiver::MutRef)
    } else if first == "&self" {
        Some(Receiver::Ref)
    } else if first == "self" || first == "mut self" {
        Some(Receiver::OwnedSelf)
    } else {
        None
    }
}

/// Detect side effects within function body lines using grammar patterns.
pub(crate) fn detect_effects(body_lines: &[(usize, &str)], contract: &ContractGrammar) -> Vec<Effect> {
    let mut effects: Vec<Effect> = Vec::new();
    let mut seen_kinds: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (effect_kind, patterns) in &contract.effects {
        for pattern in patterns {
            let re = match Regex::new(pattern) {
                Ok(r) => r,
                Err(_) => continue,
            };

            for (_line_num, text) in body_lines {
                if re.is_match(text) && seen_kinds.insert(effect_kind.clone()) {
                    let effect = match effect_kind.as_str() {
                        "file_read" => Effect::FileRead,
                        "file_write" => Effect::FileWrite,
                        "file_delete" => Effect::FileDelete,
                        "process_spawn" => {
                            // Try to extract the command name
                            let cmd = re
                                .captures(text)
                                .and_then(|c| c.get(1))
                                .map(|m| m.as_str().to_string());
                            Effect::ProcessSpawn { command: cmd }
                        }
                        "mutation" => {
                            let target = re
                                .captures(text)
                                .and_then(|c| c.get(1))
                                .map(|m| m.as_str().to_string())
                                .unwrap_or_else(|| "unknown".to_string());
                            Effect::Mutation { target }
                        }
                        "panic" => {
                            let msg = re
                                .captures(text)
                                .and_then(|c| c.get(1))
                                .map(|m| m.as_str().to_string());
                            Effect::Panic { message: msg }
                        }
                        "network" => Effect::Network,
                        "resource_alloc" => Effect::ResourceAlloc { resource: None },
                        "logging" => Effect::Logging,
                        _ => continue,
                    };
                    effects.push(effect);
                    break; // Only add each effect kind once per function
                }
            }
        }
    }

    // Also detect panics from panic_patterns
    for pattern in &contract.panic_patterns {
        if let Ok(re) = Regex::new(pattern) {
            for (_line_num, text) in body_lines {
                if re.is_match(text) && seen_kinds.insert("panic".to_string()) {
                    let msg = re
                        .captures(text)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str().to_string());
                    effects.push(Effect::Panic { message: msg });
                    break;
                }
            }
        }
    }

    effects
}

/// Detect function calls within the body and track parameter forwarding.
pub(crate) fn detect_calls(body_lines: &[(usize, &str)], params: &[Param]) -> Vec<FunctionCall> {
    let mut calls: Vec<FunctionCall> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Simple call detection: word followed by (
    static CALL_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(\w+(?:::\w+)*)\s*\(").unwrap());

    let param_names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();

    for (_line_num, text) in body_lines {
        for caps in CALL_RE.captures_iter(text) {
            let fn_name = caps[1].to_string();

            // Skip common non-function keywords
            if matches!(
                fn_name.as_str(),
                "if" | "while"
                    | "for"
                    | "match"
                    | "return"
                    | "let"
                    | "Some"
                    | "None"
                    | "Ok"
                    | "Err"
                    | "vec"
                    | "format"
                    | "println"
                    | "eprintln"
                    | "write"
                    | "writeln"
            ) {
                continue;
            }

            if !seen.insert(fn_name.clone()) {
                continue;
            }

            // Check which params are forwarded to this call
            let call_text = text.trim();
            let forwards: Vec<String> = param_names
                .iter()
                .filter(|&&p| {
                    // Check if the param name appears in the same line as the call
                    call_text.contains(p)
                })
                .map(|&p| p.to_string())
                .collect();

            calls.push(FunctionCall {
                function: fn_name,
                forwards,
            });
        }
    }

    calls
}
