use super::is_scaffolding_line;

/// Suppress low-information literal/envelope repeats: DTO tails full of
/// `None`/`Default::default()` and repeated error constructors. These are common
/// review-noise patterns where extraction usually hides branch intent.
pub(super) fn is_low_information_literal_or_error_block(
    normalized: &[(usize, String)],
    start: usize,
    len: usize,
    min_block_lines: usize,
) -> bool {
    let end = (start + len).min(normalized.len());
    if start >= end {
        return false;
    }

    let window = &normalized[start..end];
    let low_info_lines = window
        .iter()
        .filter(|(_, line)| is_low_information_literal_or_error_line(line))
        .count();

    low_info_lines >= min_block_lines && low_info_lines * 100 / window.len() >= 80
}

fn is_low_information_literal_or_error_line(normalized: &str) -> bool {
    let t = normalized.trim().trim_end_matches(',');

    if t.is_empty() || is_scaffolding_line(t) {
        return true;
    }

    if t == "0" || t == "..default::default()" {
        return true;
    }

    if is_neutral_struct_field(t) {
        return true;
    }

    if is_error_envelope_line(t) {
        return true;
    }

    if is_simple_argument_line(t) {
        return true;
    }

    false
}

fn is_simple_argument_line(line: &str) -> bool {
    let mut value = line.trim();
    value = value.strip_prefix("&mut ").unwrap_or(value);
    value = value.strip_prefix('&').unwrap_or(value).trim();

    if is_simple_identifier_path(value) {
        return true;
    }

    if value.ends_with(".clone()") || value.ends_with(".to_string()") {
        return true;
    }

    if let Some((left, right)) = value.split_once(" + ") {
        return is_simple_identifier_path(left.trim()) && right.trim().parse::<u64>().is_ok();
    }

    false
}

fn is_simple_identifier_path(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.'))
        && value.chars().any(|c| c.is_ascii_alphabetic() || c == '_')
}

fn is_neutral_struct_field(line: &str) -> bool {
    let Some((_field, value)) = line.split_once(':') else {
        return false;
    };
    let value = value.trim();

    value == "none"
        || value == "default::default()"
        || value == "false"
        || value == "0"
        || value.ends_with(".clone()")
        || value.ends_with(".to_string()")
        || value.starts_with("some(")
        || is_simple_argument_line(value)
}

fn is_error_envelope_line(line: &str) -> bool {
    line.contains("error::")
        || line.contains("::error")
        || line.contains("internal_io(")
        || line.starts_with("format!(")
        || line.starts_with("some(")
}
