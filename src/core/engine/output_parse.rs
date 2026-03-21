use regex::Regex;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Aggregate {
    First,
    Last,
    Sum,
    Max,
}

#[derive(Debug, Clone)]
pub struct ParseRule {
    pub pattern: String,
    pub field: String,
    pub group: usize,
    pub aggregate: Aggregate,
}

#[derive(Debug, Clone)]
pub struct DeriveRule {
    pub field: String,
    pub expr: String,
}

#[derive(Debug, Clone, Default)]
pub struct ParseSpec {
    pub rules: Vec<ParseRule>,
    pub defaults: HashMap<String, f64>,
    pub derive: Vec<DeriveRule>,
}

impl ParseSpec {
    pub fn parse(&self, text: &str) -> HashMap<String, f64> {
        parse_output(text, self)
    }
}

fn parse_output(text: &str, spec: &ParseSpec) -> HashMap<String, f64> {
    let mut out = spec.defaults.clone();

    for rule in &spec.rules {
        let Ok(re) = Regex::new(&rule.pattern) else {
            continue;
        };
        let matches = re
            .captures_iter(text)
            .filter_map(|caps| caps.get(rule.group))
            .filter_map(|m| m.as_str().trim().parse::<f64>().ok())
            .collect::<Vec<_>>();

        if matches.is_empty() {
            continue;
        }

        let value = match rule.aggregate {
            Aggregate::First => matches.first().copied().unwrap_or(0.0),
            Aggregate::Last => matches.last().copied().unwrap_or(0.0),
            Aggregate::Sum => matches.iter().sum(),
            Aggregate::Max => matches.iter().cloned().fold(0.0, f64::max),
        };
        out.insert(rule.field.clone(), value);
    }

    for derive in &spec.derive {
        let value = evaluate_expr(&derive.expr, &out);
        out.insert(derive.field.clone(), value);
    }

    out
}

fn evaluate_expr(expr: &str, values: &HashMap<String, f64>) -> f64 {
    let spaced = expr
        .replace('+', " + ")
        .replace('-', " - ")
        .split_whitespace()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();

    if spaced.is_empty() {
        return 0.0;
    }

    let mut total = 0.0;
    let mut op = "+";
    for token in spaced {
        if token == "+" || token == "-" {
            op = if token == "-" { "-" } else { "+" };
            continue;
        }

        let value = token
            .parse::<f64>()
            .ok()
            .or_else(|| values.get(&token).copied())
            .unwrap_or(0.0);
        if op == "-" {
            total -= value;
        } else {
            total += value;
        }
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_default_path() {
        let instance = ParseSpec::default();
        let text = "";
        let result = instance.parse(&text);
        assert!(
            !result.is_empty(),
            "expected non-empty collection for: default path"
        );
    }
}
