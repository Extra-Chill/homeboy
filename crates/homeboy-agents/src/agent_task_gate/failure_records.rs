use super::*;

/// Produce a stable failure identity from the exit status, structured diagnostic
/// identities, and the failure-identifying output records (failed test names,
/// compiler errors with their relative locations, panic locations). When no
/// failure-identifying records exist, a digest of the whole normalized output is
/// used instead. Output is evidence, not the sole authority for accepting an
/// inherited failure.
pub(crate) fn failure_fingerprint(
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    diagnostics: &[AgentTaskGateDiagnosticRecord],
) -> String {
    let normalized_lines = [stdout, stderr]
        .into_iter()
        .flat_map(str::lines)
        .filter_map(normalize_failure_record)
        .collect::<Vec<_>>();
    let mut diagnostic_ids = diagnostics
        .iter()
        .map(|diagnostic| format!("{}:{}", diagnostic.producer.id, diagnostic.identity))
        .collect::<Vec<_>>();
    diagnostic_ids.sort();
    diagnostic_ids.dedup();

    let mut failure_records = failure_identifying_records(&normalized_lines);
    failure_records.sort();
    failure_records.dedup();
    let payload = if failure_records.is_empty() {
        let mut output_records = normalized_lines
            .into_iter()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        output_records.sort();
        output_records.dedup();
        json!({
            "exit_code": exit_code,
            "diagnostic_ids": diagnostic_ids,
            "output_records": output_records,
        })
    } else {
        json!({
            "exit_code": exit_code,
            "diagnostic_ids": diagnostic_ids,
            "failure_records": failure_records,
        })
    };
    format!(
        "sha256:{:x}",
        Sha256::digest(payload.to_string().as_bytes())
    )
}

/// Stable failure records retained alongside the digest for subset comparisons.
pub(crate) fn failure_records(stdout: &str, stderr: &str) -> Vec<String> {
    let lines = [stdout, stderr]
        .into_iter()
        .flat_map(str::lines)
        .filter_map(normalize_failure_record)
        .collect::<Vec<_>>();
    let mut records = failure_identifying_records(&lines);
    records.sort();
    records.dedup();
    records
}

/// Extract the records that identify *which* failure happened from normalized
/// output lines: failed test names, compiler errors (with their `-->` location),
/// and panic locations.
pub(super) fn failure_identifying_records(lines: &[String]) -> Vec<String> {
    let patterns = failure_patterns();
    let mut records = Vec::new();
    let mut in_failures_list = false;
    let mut pending_error: Option<String> = None;
    for line in lines {
        let trimmed = line.trim();
        if let Some(error) = pending_error.as_ref() {
            if let Some(location) = trimmed.strip_prefix("--> ") {
                let location = patterns
                    .location_column
                    .replace(location.trim(), "$1")
                    .into_owned();
                records.push(format!("{error} @ {location}"));
                pending_error = None;
                continue;
            }
            if !trimmed.is_empty() {
                records.push(error.clone());
                pending_error = None;
            }
        }
        if trimmed == "failures:" {
            in_failures_list = true;
            continue;
        }
        if in_failures_list {
            let indented = line.starts_with(char::is_whitespace);
            if trimmed.is_empty() {
                continue;
            }
            if indented && !trimmed.contains(char::is_whitespace) {
                records.push(format!("test:{trimmed}"));
                continue;
            }
            in_failures_list = false;
        }
        if let Some(captures) = patterns.failed_test.captures(trimmed) {
            records.push(format!("test:{}", &captures[1]));
            continue;
        }
        if let Some(captures) = patterns.js_failed_test.captures(trimmed) {
            records.push(format!("test:{}", &captures[1]));
            continue;
        }
        if let Some(captures) = patterns.panic_location.captures(trimmed) {
            records.push(format!("panic:{}:{}", &captures[1], &captures[2]));
            continue;
        }
        if patterns.error_line.is_match(trimmed) {
            pending_error = Some(trimmed.to_string());
        }
    }
    if let Some(error) = pending_error {
        records.push(error);
    }
    records
}

pub(super) struct FailurePatterns {
    failed_test: regex::Regex,
    js_failed_test: regex::Regex,
    run_total: regex::Regex,
    panic_location: regex::Regex,
    error_line: regex::Regex,
    location_column: regex::Regex,
    lab_workspace_root: regex::Regex,
    homeboy_test_tmp: regex::Regex,
    absolute_path: regex::Regex,
    thread_id: regex::Regex,
    duration: regex::Regex,
    artifact_hash: regex::Regex,
    shifting_count: regex::Regex,
    timestamp: regex::Regex,
    address: regex::Regex,
}

pub(super) fn failure_patterns() -> &'static FailurePatterns {
    static PATTERNS: std::sync::OnceLock<FailurePatterns> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        let re = |pattern: &str| regex::Regex::new(pattern).expect("valid failure pattern");
        FailurePatterns {
            failed_test: re(r"^test (\S+) \.\.\. FAILED$"),
            js_failed_test: re(r"^(?:FAIL\s+|[×✕]\s+)(.+?)(?:\s+\(?<duration>\)?)?$"),
            run_total: re(r"^(\s*(?:Test Files|Tests)\b.*)\(\d+\)(\s*)$"),
            panic_location: re(r"panicked at ([^\s:]+):(\d+)(?::\d+)?"),
            error_line: re(r"^error(?:\[[A-Za-z0-9]+\])?: \S"),
            location_column: re(r"^(\S+?:\d+)(?::\d+)?$"),
            lab_workspace_root: re(r"/(?:[^/\s'`]+/)*_lab_workspaces/[^/\s'`]+/"),
            homeboy_test_tmp: re(
                r"/(?:[^/\s'`]+/)*\.homeboy-test-tmp/run\.[^/\s'`]+/(?:\.tmp[^/\s'`]+/)?",
            ),
            absolute_path: re(r"(^|[\s('`\x22=\[])(/[^\s'`\x22:)\]]+)"),
            thread_id: re(r"(thread '[^']*') \(\d+\)"),
            duration: re(r"\b(?:\d+h\s+)?(?:\d+m\s+)?\d+(?:\.\d+)?\s*(?:ns|µs|us|ms|s)\b|\b\d+m\b"),
            artifact_hash: re(r"-[0-9a-f]{16}\b"),
            shifting_count: re(r"\b\d+ (passed|ignored|measured|filtered out)\b"),
            timestamp: re(
                r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?",
            ),
            address: re(r"\b0x[0-9a-fA-F]+\b"),
        }
    })
}

/// Anchors that mark the start of a repository-relative path inside an
/// absolute path whose checkout root is unknown.
pub(super) const REPO_PATH_ANCHORS: &[&str] = &[
    "crates", "src", "tests", "benches", "examples", "target", "lib", "packages", "apps",
];

pub(super) fn relativize_absolute_path(path: &str) -> String {
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    if let Some(index) = segments
        .iter()
        .position(|segment| REPO_PATH_ANCHORS.contains(segment))
    {
        return segments[index..].join("/");
    }
    if let Some(index) = segments
        .iter()
        .position(|segment| *segment == "workspace" || *segment == "checkout")
    {
        if index + 1 < segments.len() {
            return segments[index + 1..].join("/");
        }
    }
    match segments.last() {
        Some(last) if !last.is_empty() => format!("<abs>/{last}"),
        _ => "<abs>".to_string(),
    }
}

/// Normalize one output line, masking volatile content (absolute paths,
/// thread ids, durations, artifact hashes, shifting counts, timestamps,
/// addresses). Returns `None` for progress lines that never identify a failure.
pub(super) fn normalize_failure_record(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    if matches!(
        first_word,
        "Compiling"
            | "Downloaded"
            | "Downloading"
            | "Running"
            | "Checking"
            | "Fresh"
            | "Updating"
            | "Locking"
            | "Blocking"
            | "Doc-tests"
    ) {
        return None;
    }
    let patterns = failure_patterns();
    let line = patterns.timestamp.replace_all(line, "<timestamp>");
    let line = patterns.lab_workspace_root.replace_all(&line, "");
    let line = patterns.homeboy_test_tmp.replace_all(&line, "");
    let line = patterns
        .absolute_path
        .replace_all(&line, |captures: &regex::Captures| {
            format!("{}{}", &captures[1], relativize_absolute_path(&captures[2]))
        });
    let line = patterns.thread_id.replace_all(&line, "$1");
    let line = patterns.artifact_hash.replace_all(&line, "-<hash>");
    let line = patterns.duration.replace_all(&line, "<duration>");
    let line = patterns.shifting_count.replace_all(&line, "<n> $1");
    let line = patterns.run_total.replace(&line, "${1}(<n>)$2");
    let line = patterns.address.replace_all(&line, "<address>");
    let leading = &line[..line.len() - line.trim_start().len()];
    let body = line.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(if leading.is_empty() || body.is_empty() {
        body
    } else {
        format!(" {body}")
    })
}
