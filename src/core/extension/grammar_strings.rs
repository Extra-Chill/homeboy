//! String-literal scanning helpers for grammar walking.

/// Check if a line opens a Rust raw string (`r#"`, `r##"`, etc.) that doesn't
/// close on the same line. Returns the closing delimiter pattern if found.
pub fn find_unclosed_raw_string_on_line(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut pos = 0;

    while pos < len {
        // Skip regular strings — don't confuse `"..."` with raw string opening.
        if bytes[pos] == b'"' && (pos == 0 || bytes[pos - 1] != b'r') {
            pos += 1;
            while pos < len {
                if bytes[pos] == b'"' && bytes[pos - 1] != b'\\' {
                    pos += 1;
                    break;
                }
                pos += 1;
            }
            continue;
        }

        // Look for r followed by one or more # then "
        if bytes[pos] == b'r' && pos + 1 < len {
            let hash_start = pos + 1;
            let mut hash_end = hash_start;
            while hash_end < len && bytes[hash_end] == b'#' {
                hash_end += 1;
            }
            let hash_count = hash_end - hash_start;

            if hash_count > 0 && hash_end < len && bytes[hash_end] == b'"' {
                let close_pattern = format!("\"{}", "#".repeat(hash_count));

                // Check if the closing pattern appears later on the SAME line
                let after_open = &line[hash_end + 1..];
                if !after_open.contains(&close_pattern) {
                    return Some(close_pattern);
                }

                // Closed on same line — skip past the close and continue
                if let Some(close_pos) = after_open.find(&close_pattern) {
                    pos = hash_end + 1 + close_pos + close_pattern.len();
                    continue;
                }
            }
        }

        pos += 1;
    }

    None
}

pub(crate) fn line_has_unclosed_regular_string(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut pos = 0;
    let mut in_string = false;

    while pos < bytes.len() {
        let byte = bytes[pos];

        if byte == b'"' && !is_escaped(bytes, pos) {
            // Raw string openers are handled separately. Treat the opening
            // delimiter as non-regular so `r#"..."#` does not toggle us.
            if !in_string && pos > 0 && bytes[pos - 1] == b'r' {
                pos += 1;
                continue;
            }
            in_string = !in_string;
        }

        pos += 1;
    }

    in_string
}

pub(crate) fn line_closes_regular_string(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() {
        if bytes[pos] == b'"' && !is_escaped(bytes, pos) {
            return true;
        }
        pos += 1;
    }

    false
}

fn is_escaped(bytes: &[u8], pos: usize) -> bool {
    if pos == 0 {
        return false;
    }

    let mut backslashes = 0;
    let mut i = pos;
    while i > 0 && bytes[i - 1] == b'\\' {
        backslashes += 1;
        i -= 1;
    }

    backslashes % 2 == 1
}
