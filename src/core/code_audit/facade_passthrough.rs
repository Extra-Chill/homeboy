//! Facade-passthrough detection.
//!
//! Flags PHP classes where most public methods are single-statement delegates
//! to the same inner member, e.g. `return $this->operations->create(...)`.

use std::collections::{HashMap, HashSet};

use regex::Regex;

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

const MIN_PUBLIC_METHODS: usize = 3;
const DELEGATE_RATIO_THRESHOLD: f32 = 0.70;

pub(super) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let call_site_counts = count_new_call_sites(fingerprints);
    let mut findings = Vec::new();

    for fp in fingerprints {
        if fp.language != Language::Php {
            continue;
        }

        let Some(class_name) = fp.type_name.as_deref() else {
            continue;
        };

        let public_methods = collect_public_methods(fp);
        if public_methods.len() < MIN_PUBLIC_METHODS {
            continue;
        }

        let mut delegate_count = 0usize;
        let mut target_members: HashMap<String, usize> = HashMap::new();

        for method in &public_methods {
            let Some(body) = extract_method_body(&fp.content, method) else {
                continue;
            };
            let Some(member) = classify_delegate(&body) else {
                continue;
            };
            delegate_count += 1;
            *target_members.entry(member).or_default() += 1;
        }

        let total_public = public_methods.len();
        let ratio = delegate_count as f32 / total_public as f32;
        if ratio < DELEGATE_RATIO_THRESHOLD {
            continue;
        }

        let total_call_sites = call_site_counts.get(class_name).copied().unwrap_or(0);
        let own_call_sites = count_new_in_content(&fp.content, class_name);
        let external_call_sites = total_call_sites.saturating_sub(own_call_sites);
        let member_list = render_member_counts(target_members);

        findings.push(Finding {
            convention: "facade_passthrough".to_string(),
            severity: Severity::Warning,
            file: fp.relative_path.clone(),
            description: format!(
                "Facade passthrough: {}/{} public methods delegate ({:.0}%) to [{}]; {} external call site(s) of `new {}(`",
                delegate_count,
                total_public,
                ratio * 100.0,
                member_list,
                external_call_sites,
                class_name
            ),
            suggestion: if external_call_sites == 0 {
                format!(
                    "No external callers construct {}. Consider deleting the wrapper and using the inner class directly.",
                    class_name
                )
            } else {
                "Inline callers to the inner member or collapse the facade back into the implementation class."
                    .to_string()
            },
            kind: AuditFinding::FacadePassthrough,
        });
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn collect_public_methods(fp: &FileFingerprint) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();

    for name in fp.method_hashes.keys().chain(fp.methods.iter()) {
        if !seen.insert(name.clone()) || is_magic_method(name) {
            continue;
        }
        if matches!(
            fp.visibility.get(name).map(String::as_str),
            Some("private" | "protected")
        ) {
            continue;
        }
        names.push(name.clone());
    }

    names
}

fn is_magic_method(name: &str) -> bool {
    name == "__construct" || name == "__destruct" || name.starts_with("__")
}

fn extract_method_body(content: &str, method: &str) -> Option<String> {
    let re = Regex::new(&format!(r"function\s+{}\s*\(", regex::escape(method))).ok()?;

    for m in re.find_iter(content) {
        let signature_start = m.start();
        let preamble =
            &content[preamble_start_for_signature(content, signature_start)..signature_start];
        if preamble.contains("private") || preamble.contains("protected") {
            continue;
        }

        let open = find_body_open_brace(content, m.end())?;
        let close = find_matching_close_brace(content, open)?;
        return Some(content[open + 1..close].to_string());
    }

    None
}

fn preamble_start_for_signature(content: &str, signature_start: usize) -> usize {
    let bytes = content.as_bytes();
    let mut i = signature_start;
    while i > 0 {
        if matches!(bytes[i - 1], b'{' | b'}' | b';') {
            return i;
        }
        i -= 1;
    }
    0
}

fn find_body_open_brace(content: &str, after_open_paren: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut i = after_open_paren;
    let mut paren_depth = 1usize;

    while i < bytes.len() && paren_depth > 0 {
        match bytes[i] {
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }

    while i < bytes.len() {
        match bytes[i] {
            b'{' => return Some(i),
            b';' => return None,
            _ => i += 1,
        }
    }
    None
}

fn find_matching_close_brace(content: &str, open: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth = 1usize;
    let mut i = open + 1;

    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            b'\'' | b'"' => i = skip_quoted(bytes, i),
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => i = skip_line(bytes, i),
            b'#' => i = skip_line(bytes, i),
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => i = skip_block_comment(bytes, i),
            _ => {}
        }
        i += 1;
    }
    None
}

fn skip_quoted(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() && bytes[i] != quote {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
        } else {
            i += 1;
        }
    }
    i
}

fn skip_line(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
        i += 1;
    }
    i + 1
}

fn classify_delegate(body: &str) -> Option<String> {
    let return_re = Regex::new(
        r"^return\s+\$this->([A-Za-z_][A-Za-z0-9_]*)->[A-Za-z_][A-Za-z0-9_]*\s*\([^;{}]*\)\s*;\s*$",
    )
    .ok()?;
    let void_re = Regex::new(
        r"^\$this->([A-Za-z_][A-Za-z0-9_]*)->[A-Za-z_][A-Za-z0-9_]*\s*\([^;{}]*\)\s*;\s*$",
    )
    .ok()?;

    let trimmed = body.trim();
    return_re
        .captures(trimmed)
        .or_else(|| void_re.captures(trimmed))
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
}

fn count_new_call_sites(fingerprints: &[&FileFingerprint]) -> HashMap<String, usize> {
    let classes: HashSet<String> = fingerprints
        .iter()
        .flat_map(|fp| fp.type_name.iter().chain(fp.type_names.iter()))
        .cloned()
        .collect();

    classes
        .into_iter()
        .map(|class| {
            let count = fingerprints
                .iter()
                .map(|fp| count_new_in_content(&fp.content, &class))
                .sum();
            (class, count)
        })
        .collect()
}

fn count_new_in_content(content: &str, class: &str) -> usize {
    Regex::new(&format!(r"\bnew\s+{}\s*\(", regex::escape(class)))
        .map(|re| re.find_iter(content).count())
        .unwrap_or(0)
}

fn render_member_counts(target_members: HashMap<String, usize>) -> String {
    let mut members: Vec<(String, usize)> = target_members.into_iter().collect();
    members.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    members
        .into_iter()
        .map(|(member, count)| format!("${}x{}", member, count))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fp(
        path: &str,
        class: &str,
        content: &str,
        methods: &[(&str, &str)],
    ) -> FileFingerprint {
        let mut visibility = HashMap::new();
        let mut method_hashes = HashMap::new();
        let mut method_names = Vec::new();

        for (name, vis) in methods {
            visibility.insert((*name).to_string(), (*vis).to_string());
            method_hashes.insert((*name).to_string(), format!("h_{}", name));
            method_names.push((*name).to_string());
        }

        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            methods: method_names,
            type_name: Some(class.to_string()),
            type_names: vec![class.to_string()],
            content: content.to_string(),
            method_hashes,
            visibility,
            ..Default::default()
        }
    }

    #[test]
    fn all_delegate_class_fires() {
        let content = r#"<?php
class Jobs {
    public function create($a) { return $this->ops->create($a); }
    public function update($a, $b) { return $this->ops->update($a, $b); }
    public function delete($a) { return $this->ops->delete($a); }
    public function find($a) { return $this->ops->find($a); }
}
"#;

        let fp = make_fp(
            "inc/Core/Database/Jobs/Jobs.php",
            "Jobs",
            content,
            &[
                ("create", "public"),
                ("update", "public"),
                ("delete", "public"),
                ("find", "public"),
            ],
        );

        let findings = run(&[&fp]);
        assert_eq!(findings.len(), 1, "expected 1 finding: {:?}", findings);
        assert_eq!(findings[0].kind, AuditFinding::FacadePassthrough);
        assert!(findings[0].description.contains("4/4"));
        assert!(findings[0].description.contains("$opsx4"));
    }

    #[test]
    fn mostly_real_class_does_not_fire() {
        let content = r#"<?php
class Service {
    public function create($a) { return $this->ops->create($a); }
    public function update($a) { return $this->ops->update($a); }
    public function compute($a) { $x = $a * 2; return $x; }
    public function transform($input) { return array_map('strtoupper', $input); }
    public function validate($input) { if (empty($input)) { throw new Exception('empty'); } return true; }
}
"#;
        let fp = make_fp(
            "inc/Service.php",
            "Service",
            content,
            &[
                ("create", "public"),
                ("update", "public"),
                ("compute", "public"),
                ("transform", "public"),
                ("validate", "public"),
            ],
        );

        assert!(run(&[&fp]).is_empty());
    }

    #[test]
    fn private_methods_and_constructor_are_ignored() {
        let content = r#"<?php
class Mixed {
    public function __construct($ops) { $this->ops = $ops; }
    public function a($x) { return $this->ops->a($x); }
    public function b($x) { return $this->ops->b($x); }
    public function c($x) { return $this->ops->c($x); }
    private function helper() { return $this->ops->helper(); }
}
"#;
        let fp = make_fp(
            "inc/Mixed.php",
            "Mixed",
            content,
            &[
                ("__construct", "public"),
                ("a", "public"),
                ("b", "public"),
                ("c", "public"),
                ("helper", "private"),
            ],
        );

        let findings = run(&[&fp]);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("3/3"));
    }

    #[test]
    fn external_constructor_call_count_is_reported() {
        let facade = make_fp(
            "inc/Facade.php",
            "Facade",
            r#"<?php
class Facade {
    public function a() { return $this->inner->a(); }
    public function b() { return $this->inner->b(); }
    public function c() { return $this->inner->c(); }
}
"#,
            &[("a", "public"), ("b", "public"), ("c", "public")],
        );
        let caller = make_fp(
            "inc/Caller.php",
            "Caller",
            "<?php class Caller { public function make() { return new Facade(); } }",
            &[("make", "public")],
        );

        let findings = run(&[&facade, &caller]);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("1 external call site"));
    }

    #[test]
    fn small_class_below_min_methods_does_not_fire() {
        let fp = make_fp(
            "inc/Tiny.php",
            "Tiny",
            "<?php class Tiny { public function a() { return $this->x->a(); } public function b() { return $this->x->b(); } }",
            &[("a", "public"), ("b", "public")],
        );

        assert!(run(&[&fp]).is_empty());
    }
}
