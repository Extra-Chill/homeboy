//! Contiguous comment-block extraction shared by comment-hygiene rules.
//!
//! Some passes (notably `upstream_workaround`) need to see comment text
//! grouped into contiguous blocks instead of per-line, because markers and
//! tracker references frequently sit on different lines of the same
//! `/** … */` docblock or `// … //` run. Per-line scanning would miss the
//! marker/reference pair entirely.
//!
//! Recognized block shapes:
//! - Contiguous `//` lines (any supported language) → one block.
//! - PHPDoc / JSDoc / C-style `/* … */` → one block.
//! - `#` lines in PHP → one block.
//! - Adjacent comment regions separated by blank lines / code → separate blocks.

use super::conventions::Language;
use super::fingerprint::FileFingerprint;

/// A contiguous block of comment lines, joined for phrase scanning. The
/// `text` field has comment markers (`//`, `*`, `#`) stripped per line so
/// substring matching is clean.
#[derive(Debug)]
pub(super) struct CommentBlock {
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
}

/// Extract every comment block from `fp`. Returns an empty vec for languages
/// without recognized comment syntax.
pub(super) fn extract(fp: &FileFingerprint) -> Vec<CommentBlock> {
    if !matches!(
        fp.language,
        Language::Php | Language::Rust | Language::JavaScript | Language::TypeScript
    ) {
        return Vec::new();
    }

    let allow_hash = matches!(fp.language, Language::Php);
    let lines: Vec<&str> = fp.content.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0usize;

    while i < lines.len() {
        let trimmed = lines[i].trim_start();

        if trimmed.starts_with("/*") {
            i = consume_block_comment(&lines, i, &mut blocks);
            continue;
        }

        if is_line_comment_start(trimmed, allow_hash) {
            i = consume_line_run(&lines, i, allow_hash, &mut blocks);
            continue;
        }

        i += 1;
    }

    blocks
}

/// Consume a `/* ... */` block starting at `start`. Returns the next index to
/// resume scanning from. Pushes one `CommentBlock` onto `blocks`.
fn consume_block_comment(lines: &[&str], start: usize, blocks: &mut Vec<CommentBlock>) -> usize {
    let trimmed = lines[start].trim_start();
    let start_line = start + 1;
    let mut text_lines: Vec<String> = Vec::new();
    let mut end_line = start_line;
    let mut first = trimmed.trim_start_matches('/').trim_start_matches('*');
    let mut closed_on_first = false;
    if let Some(idx) = first.find("*/") {
        first = &first[..idx];
        closed_on_first = true;
    }
    text_lines.push(strip_block_line(first).to_string());

    let next_index = if !closed_on_first {
        let mut j = start + 1;
        while j < lines.len() {
            end_line = j + 1;
            let l = lines[j];
            if let Some(idx) = l.find("*/") {
                text_lines.push(strip_block_line(&l[..idx]).to_string());
                break;
            }
            text_lines.push(strip_block_line(l).to_string());
            j += 1;
        }
        j + 1
    } else {
        start + 1
    };

    blocks.push(CommentBlock {
        start_line,
        end_line,
        text: text_lines.join("\n"),
    });
    next_index
}

/// Consume a contiguous run of `//` (and optionally `#`) line comments
/// starting at `start`. Returns the next index to resume scanning from.
fn consume_line_run(
    lines: &[&str],
    start: usize,
    allow_hash: bool,
    blocks: &mut Vec<CommentBlock>,
) -> usize {
    let start_line = start + 1;
    let mut text_lines: Vec<String> = Vec::new();
    let mut end_line = start_line;
    let mut j = start;
    while j < lines.len() {
        let lt = lines[j].trim_start();
        if !is_line_comment_start(lt, allow_hash) {
            break;
        }
        let stripped = lt
            .trim_start_matches('/')
            .trim_start_matches('/')
            .trim_start_matches('#')
            .trim();
        text_lines.push(stripped.to_string());
        end_line = j + 1;
        j += 1;
    }
    blocks.push(CommentBlock {
        start_line,
        end_line,
        text: text_lines.join("\n"),
    });
    j
}

fn is_line_comment_start(trimmed: &str, allow_hash: bool) -> bool {
    (trimmed.starts_with("//") && !trimmed.starts_with("///") && !trimmed.starts_with("//!"))
        || (allow_hash && trimmed.starts_with('#') && !trimmed.starts_with("#!"))
}

fn strip_block_line(line: &str) -> &str {
    line.trim().trim_start_matches('*').trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::Language;
    use crate::code_audit::fingerprint::FileFingerprint;

    fn make_fp(path: &str, lang: Language, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: lang,
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_extract_groups_contiguous_lines() {
        let fp = make_fp(
            "src/example.php",
            Language::Php,
            "<?php\n// line one\n// line two\n\n// separate block\n$x = 1;\n",
        );
        let blocks = extract(&fp);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].start_line, 2);
        assert_eq!(blocks[0].end_line, 3);
        assert!(blocks[0].text.contains("line one"));
        assert!(blocks[0].text.contains("line two"));
        assert_eq!(blocks[1].start_line, 5);
    }

    #[test]
    fn test_extract_phpdoc() {
        let fp = make_fp(
            "src/example.php",
            Language::Php,
            "<?php\n/**\n * Some doc\n * @see https://example.com/issues/1\n */\nclass A {}\n",
        );
        let blocks = extract(&fp);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].text.contains("Some doc"));
        assert!(blocks[0].text.contains("@see"));
    }

    #[test]
    fn test_extract_unknown_language_returns_empty() {
        let fp = make_fp("README", Language::Unknown, "// not a comment block\n");
        assert!(extract(&fp).is_empty());
    }
}
