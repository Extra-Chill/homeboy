//! structural_parser_context — extracted from grammar.rs.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use crate::error::{Error, Result};
use super::StringSyntax;
use super::Grammar;
use super::Region;
use super::CommentSyntax;
use super::ContextualLine;
use super::StructuralContext;


/// Iterate lines with structural context, tracking brace depth and regions.
///
/// This is the core primitive — it walks the file line-by-line, tracking
/// brace depth and whether we're inside comments or strings. Consumers
/// can then filter lines by depth, region, etc.
pub fn walk_lines<'a>(content: &'a str, grammar: &Grammar) -> Vec<ContextualLine<'a>> {
    let mut ctx = StructuralContext::new();
    let mut result = Vec::new();
    let mut in_block_comment = false;
    let mut block_comment_end = String::new();

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let depth_at_start = ctx.depth;

        // Determine region for this line
        let region = if in_block_comment {
            // Check if block comment ends on this line
            if let Some(pos) = trimmed.find(block_comment_end.as_str()) {
                // Comment ends partway through this line
                in_block_comment = false;
                let after = &trimmed[pos + block_comment_end.len()..].trim();
                if after.is_empty() {
                    Region::BlockComment
                } else {
                    // Mixed line — treat as code (conservative)
                    Region::Code
                }
            } else {
                Region::BlockComment
            }
        } else if is_line_comment(trimmed, &grammar.comments) {
            Region::LineComment
        } else {
            // Check for block comment start
            for (open, close) in &grammar.comments.block {
                if trimmed.starts_with(open.as_str())
                    && (!trimmed.contains(close.as_str()) || trimmed.ends_with(open.as_str()))
                {
                    in_block_comment = true;
                    block_comment_end = close.clone();
                }
            }
            if in_block_comment {
                Region::BlockComment
            } else {
                Region::Code
            }
        };

        // Track brace depth for code lines
        if region == Region::Code {
            update_depth(line, &grammar.blocks, &grammar.strings, &mut ctx);
        }

        result.push(ContextualLine {
            text: line,
            line_num: i + 1,
            depth: depth_at_start,
            region,
        });

        // Pop exited blocks
        ctx.pop_exited_blocks();
    }

    result
}

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
