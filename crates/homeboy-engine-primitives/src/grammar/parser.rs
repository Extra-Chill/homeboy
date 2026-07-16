//! Structural parser — context-aware iteration over source text.
//!
//! Walks source line-by-line tracking brace depth and whether the cursor is
//! inside comments or strings. Consumers filter lines by depth/region.

use super::types::{BlockSyntax, CommentSyntax, Grammar, StringSyntax};
use super::{
    find_unclosed_raw_string_on_line, line_closes_regular_string, line_has_unclosed_regular_string,
};

// ============================================================================
// Structural parser — context-aware iteration over source text
// ============================================================================

/// Region classification for a line or span of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// Normal code.
    Code,
    /// Inside a single-line comment.
    LineComment,
    /// Inside a block comment.
    BlockComment,
    /// Inside a string literal.
    StringLiteral,
}

/// Tracks structural context while parsing source text.
#[derive(Debug, Clone)]
pub struct StructuralContext {
    /// Current brace nesting depth.
    pub depth: i32,

    /// Current region (code, comment, string).
    pub region: Region,

    /// Stack of block contexts: (kind_label, depth_when_entered).
    /// Features can push/pop to track impl blocks, test modules, etc.
    pub block_stack: Vec<(String, i32)>,
}

impl StructuralContext {
    pub fn new() -> Self {
        Self {
            depth: 0,
            region: Region::Code,
            block_stack: Vec::new(),
        }
    }

    /// Whether we're inside a block with the given label.
    #[cfg(test)]
    pub(crate) fn is_inside(&self, label: &str) -> bool {
        self.block_stack.iter().any(|(l, _)| l == label)
    }

    /// The label of the innermost block, if any.
    #[cfg(test)]
    pub(crate) fn current_block_label(&self) -> Option<&str> {
        self.block_stack.last().map(|(l, _)| l.as_str())
    }

    /// Push a labeled block at the current depth.
    #[cfg(test)]
    pub(crate) fn push_block(&mut self, label: String) {
        self.block_stack.push((label, self.depth));
    }

    /// Pop blocks that have been exited (depth dropped below entry depth).
    pub(crate) fn pop_exited_blocks(&mut self) {
        while let Some((_, entry_depth)) = self.block_stack.last() {
            if self.depth <= *entry_depth {
                self.block_stack.pop();
            } else {
                break;
            }
        }
    }
}

impl Default for StructuralContext {
    fn default() -> Self {
        Self::new()
    }
}

/// A line of source with its structural context.
#[derive(Debug, Clone)]
pub struct ContextualLine<'a> {
    /// The line content.
    pub text: &'a str,

    /// 1-indexed line number.
    pub line_num: usize,

    /// Brace depth at the start of this line.
    pub depth: i32,

    /// What region this line is in.
    pub region: Region,
}

/// Iterate lines with structural context, tracking brace depth and regions.
///
/// This is the core primitive — it walks the file line-by-line, tracking
/// brace depth and whether we're inside comments or strings. Consumers
/// can then filter lines by depth, region, etc.
pub(crate) fn walk_lines<'a>(content: &'a str, grammar: &Grammar) -> Vec<ContextualLine<'a>> {
    let mut ctx = StructuralContext::new();
    let mut result = Vec::new();
    let mut in_block_comment = false;
    let mut block_comment_end = String::new();
    let mut in_raw_string = false;
    let mut raw_string_close = String::new();
    let mut in_regular_string = false;

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let depth_at_start = ctx.depth;

        // Determine region for this line
        let region = if in_raw_string {
            // Inside a multi-line raw string — check if closing delimiter appears
            if line.contains(&raw_string_close) {
                in_raw_string = false;
            }
            Region::StringLiteral
        } else if in_regular_string {
            // Inside a multi-line regular string. Some grammar profiles permit
            // newline escapes in ordinary strings, and fixture source in tests
            // commonly uses that form.
            if line_closes_regular_string(line) {
                in_regular_string = false;
            }
            Region::StringLiteral
        } else if in_block_comment {
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
                // Check for multi-line raw string opening.
                if let Some(close) = find_unclosed_raw_string_on_line(line) {
                    in_raw_string = true;
                    raw_string_close = close;
                } else if line_has_unclosed_regular_string(line) {
                    in_regular_string = true;
                }
                // The opening line itself is Code (it has real code on it);
                // subsequent lines inside the string are StringLiteral.
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
fn is_line_comment(trimmed: &str, comments: &CommentSyntax) -> bool {
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
fn update_depth(
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
