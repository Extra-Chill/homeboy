//! structural_parser_context — extracted from grammar.rs.

use super::CommentSyntax;
use super::StringSyntax;
use super::super::*;


/// Check if a trimmed line is a single-line comment.
pub(crate) fn is_line_comment(trimmed: &str, comments: &CommentSyntax) -> bool {
    for prefix in &comments.line {
        if trimmed.starts_with(prefix.as_str()) {
            return true;
        }
    }
    for prefix in &comments.doc {
        if trimmed.starts_with(prefix.as_str()) {
            return true;
        }
    }
    false
}

/// Update brace depth for a line, skipping strings.
pub(crate) fn update_depth(
    line: &str,
    blocks: &BlockSyntax,
    strings: &StringSyntax,
    ctx: &mut StructuralContext,
) {
    let mut in_string: Option<char> = None;
    let mut prev_char = '\0';

    for ch in line.chars() {
        if let Some(quote) = in_string {
            if ch == quote && prev_char != strings.escape.chars().next().unwrap_or('\\') {
                in_string = None;
            }
        } else if strings.quotes.iter().any(|q| q.starts_with(ch)) {
            in_string = Some(ch);
        } else if blocks.open.starts_with(ch) {
            ctx.depth += 1;
        } else if blocks.close.starts_with(ch) {
            ctx.depth -= 1;
        }
        prev_char = ch;
    }
}
