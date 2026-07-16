use homeboy::core::git::{IssueCloseReason, IssueState, PrState};

pub(super) const BODY_STDIN_LIMIT_BYTES: u64 = 256 * 1024;

// ---------------------------------------------------------------------------
// Small input helpers
// ---------------------------------------------------------------------------

/// Resolve a body argument from either inline `--body` or a file path.
/// Returns `Ok(None)` if neither is set. Supports `-` for stdin.
pub(super) fn resolve_body(
    inline: Option<String>,
    file: Option<String>,
) -> homeboy::core::Result<Option<String>> {
    if let Some(body) = inline {
        return Ok(Some(body));
    }
    let Some(path) = file else {
        return Ok(None);
    };

    if path == "-" {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(BODY_STDIN_LIMIT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                homeboy::core::Error::internal_io(
                    format!("Failed to read body from stdin: {}", e),
                    Some("stdin".into()),
                )
            })?;
        if bytes.len() > BODY_STDIN_LIMIT_BYTES as usize {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "body_file",
                "stdin body exceeds the retained byte limit",
                Some(format!("limit_bytes={BODY_STDIN_LIMIT_BYTES}")),
                None,
            ));
        }
        let buf = String::from_utf8(bytes).map_err(|e| {
            homeboy::core::Error::internal_io(
                format!("Failed to decode body from stdin as UTF-8: {}", e),
                Some("stdin".into()),
            )
        })?;
        return Ok(Some(buf));
    }

    let content = std::fs::read_to_string(&path).map_err(|e| {
        homeboy::core::Error::internal_io(
            format!("Failed to read body file: {}", e),
            Some(path.clone()),
        )
    })?;
    Ok(Some(content))
}

pub(super) fn read_lines_file(path: &str) -> homeboy::core::Result<Vec<String>> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        homeboy::core::Error::internal_io(
            format!("Failed to read lines file: {}", e),
            Some(path.to_string()),
        )
    })?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect())
}

pub(super) fn parse_issue_state(s: &str) -> homeboy::core::Result<IssueState> {
    match s {
        "open" => Ok(IssueState::Open),
        "closed" => Ok(IssueState::Closed),
        "all" => Ok(IssueState::All),
        other => Err(homeboy::core::Error::validation_invalid_argument(
            "state",
            format!("Unknown issue state '{}'", other),
            None,
            Some(vec!["Use one of: open, closed, all".into()]),
        )),
    }
}

pub(super) fn parse_issue_close_reason(s: &str) -> homeboy::core::Result<IssueCloseReason> {
    // Accept both kebab-case (CLI ergonomic) and snake_case (matches GitHub
    // GraphQL state_reason values for symmetry with `--json stateReason`).
    match s {
        "completed" => Ok(IssueCloseReason::Completed),
        "not-planned" | "not_planned" => Ok(IssueCloseReason::NotPlanned),
        other => Err(homeboy::core::Error::validation_invalid_argument(
            "reason",
            format!("Unknown close reason '{}'", other),
            None,
            Some(vec!["Use one of: completed, not-planned".into()]),
        )),
    }
}

pub(super) fn parse_pr_state(s: &str) -> homeboy::core::Result<PrState> {
    match s {
        "open" => Ok(PrState::Open),
        "closed" => Ok(PrState::Closed),
        "merged" => Ok(PrState::Merged),
        "all" => Ok(PrState::All),
        other => Err(homeboy::core::Error::validation_invalid_argument(
            "state",
            format!("Unknown PR state '{}'", other),
            None,
            Some(vec!["Use one of: open, closed, merged, all".into()]),
        )),
    }
}
