//! Repeated enum-dispatch contract detection.
//!
//! Finds repeated exhaustive `match` blocks over the same locally-defined enum
//! where each block maps variants to primitive values or parallel getter calls.
//! These are good candidates for enum-owned behavior because adding a variant
//! otherwise requires updating scattered dispatch contracts.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::core::engine::codebase_scan::CodebaseSnapshot;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};

const MIN_MATCHES: usize = 2;

/// Run enum-dispatch contract detection over the shared audit source snapshot.
///
/// Consumes the in-memory `(path, content)` view built once during discovery
/// instead of re-walking and re-reading the tree. Behavior is preserved: the
/// snapshot is a superset of the `.rs` files this detector previously walked,
/// so filtering to Rust source files here yields the identical input set in
/// walk order.
pub(in crate::core::code_audit) fn run(snapshot: &CodebaseSnapshot) -> Vec<Finding> {
    let root = snapshot.root();
    let mut files = Vec::new();

    for (file_path, content) in snapshot.iter() {
        if file_path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Ok(relative_path) = file_path.strip_prefix(root) else {
            continue;
        };
        let relative_path = relative_path.to_string_lossy().replace('\\', "/");
        if super::walker::is_test_path(&relative_path) {
            continue;
        }
        files.push(SourceFile {
            relative_path,
            content: content.to_string(),
        });
    }

    let refs = files.iter().collect::<Vec<_>>();
    detect_repeated_enum_dispatch_contracts(&refs)
}

#[derive(Debug, Clone)]
struct SourceFile {
    relative_path: String,
    content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnumDef {
    variants: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchContract {
    enum_name: String,
    variants: BTreeSet<String>,
    file: String,
    function: String,
    body_shape: BodyShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyShape {
    Primitive,
    GetterCall,
    Constructor,
}

impl BodyShape {
    fn display_name(self) -> &'static str {
        match self {
            BodyShape::Primitive => "primitive values",
            BodyShape::GetterCall => "parallel getter calls",
            BodyShape::Constructor => "constructor-shaped outputs",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    enum_name: String,
    variants: Vec<String>,
}

fn detect_repeated_enum_dispatch_contracts(files: &[&SourceFile]) -> Vec<Finding> {
    let enum_defs = SourceFile::collect_local_enums(files);
    let mut groups: BTreeMap<GroupKey, Vec<MatchContract>> = BTreeMap::new();

    for file in files {
        for contract in
            SourceFile::extract_match_contracts(&file.content, &file.relative_path, &enum_defs)
        {
            let key = GroupKey {
                enum_name: contract.enum_name.clone(),
                variants: contract.variants.iter().cloned().collect(),
            };
            groups.entry(key).or_default().push(contract);
        }
    }

    let mut findings = Vec::new();

    for (key, mut contracts) in groups {
        if contracts.len() < MIN_MATCHES {
            continue;
        }

        contracts.sort_by(|a, b| a.file.cmp(&b.file).then(a.function.cmp(&b.function)));
        contracts.dedup_by(|a, b| a.file == b.file && a.function == b.function);
        if contracts.len() < MIN_MATCHES {
            continue;
        }
        let sites = contracts
            .iter()
            .map(|contract| format!("{}::{}", contract.file, contract.function))
            .collect::<Vec<_>>();
        let shapes = contracts
            .iter()
            .map(|contract| contract.body_shape.display_name())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let anchor = contracts
            .first()
            .map(|contract| contract.file.clone())
            .unwrap_or_else(|| "<unknown>".to_string());

        findings.push(Finding {
            convention: "enum_dispatch_contracts".to_string(),
            severity: Severity::Info,
            file: anchor,
            description: format!(
                "Repeated exhaustive matches over enum `{}` cover [{}] in {} locations: {}; body shapes: {}",
                key.enum_name,
                key.variants.join(", "),
                contracts.len(),
                sites.join(", "),
                shapes.join(", ")
            ),
            suggestion: format!(
                "Move the repeated label/getter/policy contract onto `impl {}` methods so variant additions stay localized",
                key.enum_name
            ),
            kind: AuditFinding::RepeatedEnumDispatchContract,
        });
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

impl SourceFile {
    fn collect_local_enums(files: &[&SourceFile]) -> HashMap<String, EnumDef> {
        let mut enums = HashMap::new();

        for file in files {
            for (name, variants) in Self::extract_enum_defs(&file.content) {
                enums.insert(name, EnumDef { variants });
            }
        }

        enums
    }

    fn extract_enum_defs(content: &str) -> Vec<(String, BTreeSet<String>)> {
        let mut enums = Vec::new();
        let bytes = content.as_bytes();
        let mut i = 0;

        while let Some(enum_pos) = Self::find_keyword(bytes, i, b"enum") {
            let mut cursor = enum_pos + "enum".len();
            Self::skip_ws(bytes, &mut cursor);
            let Some((name, after_name)) = Self::parse_ident(bytes, cursor) else {
                i = enum_pos + 4;
                continue;
            };
            cursor = after_name;
            Self::skip_ws(bytes, &mut cursor);
            if cursor >= bytes.len() || bytes[cursor] != b'{' {
                i = cursor;
                continue;
            }
            let Some(end) = Self::matching_brace(bytes, cursor) else {
                break;
            };
            let variants = Self::extract_enum_variants(&content[cursor + 1..end]);
            if !variants.is_empty() {
                enums.push((name, variants));
            }
            i = end + 1;
        }

        enums
    }

    fn extract_enum_variants(body: &str) -> BTreeSet<String> {
        Self::split_top_level(body, b',')
            .into_iter()
            .filter_map(|segment| {
                let trimmed = segment.trim();
                if trimmed.is_empty() {
                    return None;
                }
                let first = trimmed
                    .split(|ch: char| !Self::is_ident_char(ch as u8))
                    .find(|part| !part.is_empty())?;
                if first.chars().next()?.is_ascii_uppercase() {
                    Some(first.to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    fn extract_match_contracts(
        content: &str,
        file: &str,
        enum_defs: &HashMap<String, EnumDef>,
    ) -> Vec<MatchContract> {
        let bytes = content.as_bytes();
        let mut contracts = Vec::new();
        let mut i = 0;

        while let Some(match_pos) = Self::find_keyword(bytes, i, b"match") {
            let Some(open_brace) =
                Self::find_next_top_level_byte(bytes, match_pos + "match".len(), b'{')
            else {
                break;
            };
            let Some(close_brace) = Self::matching_brace(bytes, open_brace) else {
                break;
            };

            let function = Self::enclosing_function_name(&content[..match_pos]);
            let body = &content[open_brace + 1..close_brace];
            if let Some(contract) = Self::parse_match_body(body, file, &function, enum_defs) {
                contracts.push(contract);
            }

            i = close_brace + 1;
        }

        contracts
    }

    fn parse_match_body(
        body: &str,
        file: &str,
        function: &str,
        enum_defs: &HashMap<String, EnumDef>,
    ) -> Option<MatchContract> {
        let arms = Self::split_top_level(body, b',');
        let mut enum_name: Option<String> = None;
        let mut variants = BTreeSet::new();
        let mut shapes = Vec::new();

        for arm in arms {
            let arm = arm.trim();
            if arm.is_empty() {
                continue;
            }
            let arrow = Self::find_top_level_arrow(arm.as_bytes())?;
            let pattern = arm[..arrow].trim();
            let expression = arm[arrow + 2..].trim();
            let (arm_enum, variant) = Self::parse_enum_variant_pattern(pattern)?;
            if let Some(existing) = &enum_name {
                if existing != &arm_enum {
                    return None;
                }
            } else {
                enum_name = Some(arm_enum);
            }
            variants.insert(variant);
            shapes.push(Self::classify_body(expression)?);
        }

        let enum_name = enum_name?;
        let enum_def = enum_defs.get(&enum_name)?;
        if variants != enum_def.variants {
            return None;
        }
        if variants.len() < 2 || shapes.is_empty() {
            return None;
        }

        Some(MatchContract {
            enum_name,
            variants,
            file: file.to_string(),
            function: function.to_string(),
            body_shape: Self::dominant_shape(&shapes)?,
        })
    }

    fn parse_enum_variant_pattern(pattern: &str) -> Option<(String, String)> {
        let pattern = pattern.split(" if ").next()?.trim();
        if pattern == "_" || pattern.contains('|') {
            return None;
        }
        let path = pattern.split('(').next()?.trim();
        let mut parts = path.rsplitn(2, "::");
        let variant = parts.next()?.trim();
        let enum_path = parts.next()?.trim();
        let enum_name = enum_path.rsplit("::").next()?.trim();
        if !Self::is_upper_ident(variant) || !Self::is_upper_ident(enum_name) {
            return None;
        }
        Some((enum_name.to_string(), variant.to_string()))
    }

    fn classify_body(expression: &str) -> Option<BodyShape> {
        let expression = expression.trim().trim_end_matches(',').trim();
        if expression.is_empty() || expression.contains("=>") {
            return None;
        }
        if Self::is_primitive_expression(expression) {
            return Some(BodyShape::Primitive);
        }
        if Self::is_getter_call(expression) {
            return Some(BodyShape::GetterCall);
        }
        if Self::is_constructor_shape(expression) {
            return Some(BodyShape::Constructor);
        }
        None
    }

    fn dominant_shape(shapes: &[BodyShape]) -> Option<BodyShape> {
        let first = *shapes.first()?;
        if shapes.iter().all(|shape| *shape == first) {
            Some(first)
        } else {
            None
        }
    }

    fn is_primitive_expression(expression: &str) -> bool {
        expression.starts_with('"')
            || expression.starts_with('\'')
            || matches!(expression, "true" | "false" | "None")
            || expression.parse::<i64>().is_ok()
    }

    fn is_getter_call(expression: &str) -> bool {
        if !expression.ends_with(')') || expression.contains('{') || expression.contains(';') {
            return false;
        }
        let Some(open) = expression.find('(') else {
            return false;
        };
        let callee = expression[..open].trim();
        callee.contains('.') && callee.rsplit('.').next().is_some_and(Self::is_lower_ident)
    }

    fn is_constructor_shape(expression: &str) -> bool {
        let Some(open) = expression.find('{') else {
            return false;
        };
        expression.ends_with('}') && Self::is_upper_ident(expression[..open].trim())
    }

    fn enclosing_function_name(prefix: &str) -> String {
        let bytes = prefix.as_bytes();
        let mut cursor = bytes.len();
        while cursor > 0 {
            let search = &bytes[..cursor];
            let Some(pos) = Self::find_keyword_rev(search, b"fn") else {
                break;
            };
            let mut name_start = pos + 2;
            Self::skip_ws(bytes, &mut name_start);
            if let Some((name, _)) = Self::parse_ident(bytes, name_start) {
                return name;
            }
            cursor = pos;
        }
        "<unknown>".to_string()
    }

    fn find_keyword(bytes: &[u8], mut start: usize, keyword: &[u8]) -> Option<usize> {
        while start + keyword.len() <= bytes.len() {
            let rel = bytes[start..]
                .windows(keyword.len())
                .position(|w| w == keyword)?;
            let pos = start + rel;
            if Self::keyword_at(bytes, pos, keyword) {
                return Some(pos);
            }
            start = pos + keyword.len();
        }
        None
    }

    fn find_keyword_rev(bytes: &[u8], keyword: &[u8]) -> Option<usize> {
        if bytes.len() < keyword.len() {
            return None;
        }
        let mut pos = bytes.len() - keyword.len();
        loop {
            if Self::keyword_at(bytes, pos, keyword) {
                return Some(pos);
            }
            if pos == 0 {
                break;
            }
            pos -= 1;
        }
        None
    }

    fn keyword_at(bytes: &[u8], pos: usize, keyword: &[u8]) -> bool {
        if pos + keyword.len() > bytes.len() || &bytes[pos..pos + keyword.len()] != keyword {
            return false;
        }
        let before_ok = pos == 0 || !Self::is_ident_char(bytes[pos - 1]);
        let after = pos + keyword.len();
        let after_ok = after >= bytes.len() || !Self::is_ident_char(bytes[after]);
        before_ok && after_ok
    }

    fn find_next_top_level_byte(bytes: &[u8], start: usize, needle: u8) -> Option<usize> {
        let mut paren = 0i32;
        let mut bracket = 0i32;
        let mut angle = 0i32;
        let mut i = start;
        while i < bytes.len() {
            if Self::advance_past_string_or_comment(bytes, &mut i) {
                continue;
            }
            match bytes[i] {
                b'(' => paren += 1,
                b')' => paren -= 1,
                b'[' => bracket += 1,
                b']' => bracket -= 1,
                b'<' => angle += 1,
                b'>' => angle -= 1,
                b if b == needle && paren == 0 && bracket == 0 && angle == 0 => return Some(i),
                _ => {}
            }
            i += 1;
        }
        None
    }

    fn matching_brace(bytes: &[u8], open: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = open;
        loop {
            if i >= bytes.len() {
                return None;
            }
            if Self::advance_past_string_or_comment(bytes, &mut i) {
                continue;
            }
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn split_top_level(content: &str, separator: u8) -> Vec<&str> {
        let bytes = content.as_bytes();
        let mut parts = Vec::new();
        let mut start = 0;
        let mut depth = 0i32;
        let mut i = 0;
        while i < bytes.len() {
            if Self::advance_past_string_or_comment(bytes, &mut i) {
                continue;
            }
            match bytes[i] {
                b'{' | b'(' | b'[' => depth += 1,
                b'}' | b')' | b']' => depth -= 1,
                b if b == separator && depth == 0 => {
                    parts.push(&content[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        if start < content.len() {
            parts.push(&content[start..]);
        }
        parts
    }

    fn find_top_level_arrow(bytes: &[u8]) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = 0;
        while i + 1 < bytes.len() {
            if Self::advance_past_string_or_comment(bytes, &mut i) {
                continue;
            }
            match bytes[i] {
                b'{' | b'(' | b'[' => depth += 1,
                b'}' | b')' | b']' => depth -= 1,
                b'=' if depth == 0 && bytes[i + 1] == b'>' => return Some(i),
                _ => {}
            }
            i += 1;
        }
        None
    }

    fn advance_past_string_or_comment(bytes: &[u8], index: &mut usize) -> bool {
        if let Some(next) = Self::skip_string_or_comment(bytes, *index) {
            *index = next;
            true
        } else {
            false
        }
    }

    fn skip_string_or_comment(bytes: &[u8], i: usize) -> Option<usize> {
        if i >= bytes.len() {
            return None;
        }
        match bytes[i] {
            b'"' | b'\'' => Self::skip_quoted(bytes, i, bytes[i]),
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => Some(
                bytes[i..]
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(bytes.len(), |p| i + p + 1),
            ),
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => bytes[i + 2..]
                .windows(2)
                .position(|w| w == b"*/")
                .map(|p| i + 2 + p + 2)
                .or(Some(bytes.len())),
            _ => None,
        }
    }

    fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> Option<usize> {
        let mut i = start + 1;
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == quote {
                return Some(i + 1);
            }
            i += 1;
        }
        Some(bytes.len())
    }

    fn skip_ws(bytes: &[u8], cursor: &mut usize) {
        while *cursor < bytes.len() && bytes[*cursor].is_ascii_whitespace() {
            *cursor += 1;
        }
    }

    fn parse_ident(bytes: &[u8], start: usize) -> Option<(String, usize)> {
        if start >= bytes.len() || !Self::is_ident_start(bytes[start]) {
            return None;
        }
        let mut end = start + 1;
        while end < bytes.len() && Self::is_ident_char(bytes[end]) {
            end += 1;
        }
        Some((String::from_utf8_lossy(&bytes[start..end]).to_string(), end))
    }

    fn is_upper_ident(value: &str) -> bool {
        value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_uppercase())
            && Self::is_ident(value)
    }

    fn is_lower_ident(value: &str) -> bool {
        value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || *byte == b'_')
            && Self::is_ident(value)
    }

    fn is_ident(value: &str) -> bool {
        value
            .as_bytes()
            .iter()
            .all(|byte| Self::is_ident_char(*byte))
    }

    fn is_ident_start(byte: u8) -> bool {
        byte.is_ascii_alphabetic() || byte == b'_'
    }

    fn is_ident_char(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'_'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_file(path: &str, content: &str) -> SourceFile {
        SourceFile {
            relative_path: path.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn test_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("src dir");
        std::fs::write(
            src.join("capability.rs"),
            r#"
            enum Capability {
                Lint,
                Test,
            }

            fn label(capability: Capability) -> &'static str {
                match capability {
                    Capability::Lint => "lint",
                    Capability::Test => "test",
                }
            }

            fn has_support(capability: Capability, manifest: Manifest) -> bool {
                match capability {
                    Capability::Lint => manifest.has_lint(),
                    Capability::Test => manifest.has_test(),
                }
            }
            "#,
        )
        .expect("write fixture");

        use crate::core::engine::codebase_scan::{ExtensionFilter, ScanConfig};
        let snapshot = CodebaseSnapshot::build(
            dir.path(),
            &ScanConfig {
                extensions: ExtensionFilter::Only(vec!["rs".to_string()]),
                ..Default::default()
            },
        );
        let findings = run(&snapshot);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::RepeatedEnumDispatchContract);
    }

    #[test]
    fn detects_repeated_exhaustive_enum_dispatch_contracts() {
        let file = source_file(
            "src/capability.rs",
            r#"
            enum ExtensionCapability {
                Lint,
                Test,
                Build,
            }

            fn label(capability: Capability) -> &'static str {
                match capability {
                    ExtensionCapability::Lint => "lint",
                    ExtensionCapability::Test => "test",
                    ExtensionCapability::Build => "build",
                }
            }

            fn has_support(capability: Capability, manifest: Manifest) -> bool {
                match capability {
                    ExtensionCapability::Lint => manifest.has_lint(),
                    ExtensionCapability::Test => manifest.has_test(),
                    ExtensionCapability::Build => manifest.has_build(),
                }
            }

            fn script_path(capability: Capability, manifest: Manifest) -> Option<&str> {
                match capability {
                    ExtensionCapability::Lint => manifest.lint_script(),
                    ExtensionCapability::Test => manifest.test_script(),
                    ExtensionCapability::Build => manifest.build_script(),
                }
            }
            "#,
        );
        let findings = detect_repeated_enum_dispatch_contracts(&[&file]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::RepeatedEnumDispatchContract);
        assert!(findings[0].description.contains("ExtensionCapability"));
        assert!(findings[0].description.contains("label"));
        assert!(findings[0].description.contains("has_support"));
        assert!(findings[0].description.contains("script_path"));
    }

    #[test]
    fn ignores_non_exhaustive_or_external_enum_matches() {
        let file = source_file(
            "src/capability.rs",
            r#"
            enum Capability {
                Lint,
                Test,
                Build,
            }

            fn label(capability: Capability) -> &'static str {
                match capability {
                    Capability::Lint => "lint",
                    Capability::Test => "test",
                }
            }
            fn external(kind: Other) -> &'static str {
                match kind {
                    Other::One => "one",
                    Other::Two => "two",
                }
            }
            "#,
        );

        assert!(detect_repeated_enum_dispatch_contracts(&[&file]).is_empty());
    }

    #[test]
    fn ignores_single_exhaustive_match() {
        let file = source_file(
            "src/capability.rs",
            r#"
            enum Capability {
                Lint,
                Test,
            }

            fn label(capability: Capability) -> &'static str {
                match capability {
                    Capability::Lint => "lint",
                    Capability::Test => "test",
                }
            }
            "#,
        );

        assert!(detect_repeated_enum_dispatch_contracts(&[&file]).is_empty());
    }
}
