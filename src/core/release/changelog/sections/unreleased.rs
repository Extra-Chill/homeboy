//! unreleased — extracted from sections.rs.

use crate::engine::text;


/// Count bullet items in the unreleased section.
/// Returns 0 if no unreleased section exists or section is empty.
pub fn count_unreleased_entries(content: &str, aliases: &[String]) -> usize {
    let lines: Vec<&str> = content.lines().collect();
    let start = match find_next_section_start(&lines, aliases) {
        Some(idx) => idx,
        None => return 0,
    };

    let end = find_section_end(&lines, start);

    lines[start + 1..end]
        .iter()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("- ") || trimmed.starts_with("* ")
        })
        .count()
}

/// Extract bullet item text from the unreleased section.
/// Returns normalized bullet content without the leading marker.
pub fn get_unreleased_entries(content: &str, aliases: &[String]) -> Vec<String> {
    let lines: Vec<&str> = content.lines().collect();
    let start = match find_next_section_start(&lines, aliases) {
        Some(idx) => idx,
        None => return vec![],
    };

    let end = find_section_end(&lines, start);

    lines[start + 1..end]
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("- ") {
                Some(rest.trim().to_string())
            } else {
                trimmed
                    .strip_prefix("* ")
                    .map(|rest| rest.trim().to_string())
            }
        })
        .collect()
}
