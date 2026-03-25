//! split_params — extracted from contract_extract.rs.

use regex::Regex;
use crate::extension::grammar::{self, ContractGrammar, Grammar, Region};
use super::super::contract::*;


/// Parse function parameters from the params string.
pub(crate) fn parse_params(params_str: &str, param_format: &str) -> Vec<Param> {
    let params_str = params_str.trim();
    if params_str.is_empty() {
        return vec![];
    }

    let mut params = Vec::new();

    for part in split_params(params_str) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        match param_format {
            "type_dollar_name" => {
                // PHP format: `Type $name`, `?Type $name`, `$name`, `Type $name = default`
                // Skip $this
                if part.starts_with("$this") {
                    continue;
                }

                // Check for default value
                let (part_no_default, has_default) = if let Some(eq_pos) = part.find('=') {
                    (part[..eq_pos].trim(), true)
                } else {
                    (part, false)
                };

                if let Some(dollar_pos) = part_no_default.rfind('$') {
                    let name = part_no_default[dollar_pos + 1..].trim().to_string();
                    let type_part = part_no_default[..dollar_pos].trim();
                    let param_type = if type_part.is_empty() {
                        "mixed".to_string()
                    } else {
                        type_part.to_string()
                    };
                    params.push(Param {
                        name,
                        param_type,
                        mutable: false,
                        has_default,
                    });
                }
            }
            _ => {
                // Rust/default format: `name: Type`, `&self`, `mut name: Type`
                // Skip self/receiver params
                if part == "self" || part == "&self" || part == "&mut self" || part == "mut self" {
                    continue;
                }

                if let Some(colon_pos) = part.find(':') {
                    let name = part[..colon_pos]
                        .trim()
                        .trim_start_matches("mut ")
                        .to_string();
                    let param_type = part[colon_pos + 1..].trim().to_string();
                    let mutable = part.starts_with("mut ") || param_type.starts_with("&mut ");
                    params.push(Param {
                        name,
                        param_type,
                        mutable,
                        has_default: false,
                    });
                }
            }
        }
    }

    params
}

/// Split parameter string by commas, respecting generic angle brackets.
pub(crate) fn split_params(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0;

    for ch in s.chars() {
        match ch {
            '<' => {
                depth += 1;
                current.push(ch);
            }
            '>' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                parts.push(current.clone());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }

    parts
}
