//! Mention protection for GitHub-bound markdown.

/// Neutralize mentions in prose, preserving fenced and inline code and emails.
/// Unmatched inline backticks are prose, not a license to send notifications.
pub fn neutralize_mentions(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut fence: Option<(char, usize)> = None;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        let marker = trimmed.chars().next().unwrap_or('\0');
        let count = trimmed.chars().take_while(|&c| c == marker).count();
        if let Some((ch, len)) = fence {
            out.push_str(line);
            if indent <= 3 && marker == ch && count >= len && trimmed[count..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if indent <= 3
            && matches!(marker, '`' | '~')
            && count >= 3
            && (marker != '`' || !trimmed[count..].contains('`'))
        {
            // Fence info strings are not code: protect them too.
            out.push_str(&neutralize_prose(line));
            fence = Some((marker, count));
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find('`') {
            out.push_str(&neutralize_prose(&rest[..start]));
            let escaped = rest[..start]
                .bytes()
                .rev()
                .take_while(|&b| b == b'\\')
                .count()
                % 2
                == 1;
            rest = &rest[start..];
            if escaped {
                out.push('`');
                rest = &rest[1..];
                continue;
            }
            let len = rest.bytes().take_while(|&b| b == b'`').count();
            let mut end = None;
            let mut pos = len;
            while let Some(next) = rest[pos..].find('`') {
                let at = pos + next;
                let run = rest[at..].bytes().take_while(|&b| b == b'`').count();
                if run == len {
                    end = Some(at + run);
                    break;
                }
                pos = at + run;
            }
            if let Some(end) = end {
                out.push_str(&rest[..end]);
                rest = &rest[end..];
            } else {
                out.push_str(&neutralize_prose(rest));
                rest = "";
                break;
            }
        }
        out.push_str(&neutralize_prose(rest));
    }
    out
}

fn neutralize_prose(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous = None;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        out.push(ch);
        if ch == '@'
            && chars.peek() != Some(&'\u{200b}')
            && !previous.is_some_and(|c: char| c.is_alphanumeric() || c == '_')
        {
            out.push('\u{200b}');
        }
        previous = Some(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_code_and_emails_but_neutralizes_mentions() {
        let input = "@person a@b.com `@inline` ``@tick`code``\n````text\n@fenced\n```\n@still_fenced\n````\n~~~\n@tilde\n~~~\nhello @org/team and `unclosed @stranger\n";
        let expected = "@\u{200b}person a@b.com `@inline` ``@tick`code``\n````text\n@fenced\n```\n@still_fenced\n````\n~~~\n@tilde\n~~~\nhello @\u{200b}org/team and `unclosed @\u{200b}stranger\n";
        assert_eq!(neutralize_mentions(input), expected);
        assert_eq!(neutralize_mentions(expected), expected);
    }
}
