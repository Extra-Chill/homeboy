//! Read-only process-tree activity sampling.
//!
//! [`crate::process`] owns the *lifecycle* half of the process tree (signalling,
//! scopes, termination). This module owns the *observability* half: answering
//! "what is this process tree actually doing right now?" without changing it.
//!
//! The motivating case is diagnosing a stalled Cook. Before this existed the
//! only way to learn that a provider agent was compiling instead of editing was
//! `ps aux | grep` on the host (#11482). The same `ps -axo` walk that
//! [`crate::process`] already uses for descendant discovery answers it, so the
//! sampling is a plain read of the process table with no signals, no /proc
//! assumptions, and no platform-specific parsing beyond `ps` itself.
//!
//! Everything here is bounded on purpose: one `ps` invocation, a truncated
//! command string, and a single selected descendant. Diagnostics that scale
//! with the size of the process tree are how observability turns into a flood.

#[cfg(unix)]
use std::process::Command;

/// Maximum characters retained from a sampled command line.
///
/// Provider command lines embed the whole task prompt, so an untruncated
/// sample would push kilobytes into every heartbeat and durable progress
/// record. The retained prefix is where the actionable part lives — the program
/// and its leading arguments — so truncate rather than drop.
pub const MAX_ACTIVITY_COMMAND_CHARS: usize = 200;

/// One row of the process table, as sampled from `ps`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessActivityRow {
    pub pid: u32,
    pub ppid: u32,
    /// Wall-clock seconds since this process started.
    pub elapsed_seconds: u64,
    pub command: String,
}

/// The descendant selected as "what the tree is currently doing".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescendantActivity {
    pub pid: u32,
    /// Hops from the observed owner. `1` is a direct child (for a Cook, the
    /// provider process itself); `2` and deeper is work the provider spawned.
    pub depth: usize,
    pub elapsed_seconds: u64,
    /// Command line, truncated to [`MAX_ACTIVITY_COMMAND_CHARS`].
    pub command: String,
    /// Total descendants observed under the owner, so a reader can tell a lone
    /// process from a busy tree without being handed the whole tree.
    pub descendant_count: usize,
}

/// Parse `ps -axo pid=,ppid=,etime=,args=` output into activity rows.
///
/// Kept separate from the `ps` invocation so selection behavior is testable on
/// every platform, including ones where the probe itself returns nothing.
pub fn parse_process_activity_rows(ps_output: &str) -> Vec<ProcessActivityRow> {
    ps_output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let etime = fields.next()?;
            let elapsed_seconds = parse_ps_elapsed_seconds(etime)?;
            // `args` is the remainder of the line, spaces included. Recover it
            // from the original line rather than re-joining split fields so
            // internal spacing survives.
            let command = remainder_after_fields(line, 3).trim().to_string();
            Some(ProcessActivityRow {
                pid,
                ppid,
                elapsed_seconds,
                command,
            })
        })
        .collect()
}

/// Skip `count` whitespace-separated fields and return the rest of the line.
fn remainder_after_fields(line: &str, count: usize) -> &str {
    let mut rest = line.trim_start();
    for _ in 0..count {
        match rest.find(char::is_whitespace) {
            Some(index) => rest = rest[index..].trim_start(),
            None => return "",
        }
    }
    rest
}

/// Parse a POSIX `ps` `etime` value (`[[dd-]hh:]mm:ss`) into seconds.
pub fn parse_ps_elapsed_seconds(etime: &str) -> Option<u64> {
    let etime = etime.trim();
    if etime.is_empty() {
        return None;
    }
    let (days, clock) = match etime.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, etime),
    };
    let mut parts = clock.split(':').rev();
    let seconds: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next().map(str::parse).transpose().ok()?.unwrap_or(0);
    let hours: u64 = parts.next().map(str::parse).transpose().ok()?.unwrap_or(0);
    if parts.next().is_some() {
        return None;
    }
    Some(days * 86_400 + hours * 3_600 + minutes * 60 + seconds)
}

/// Truncate a command line to [`MAX_ACTIVITY_COMMAND_CHARS`] characters.
///
/// Truncation is on `char` boundaries so a multi-byte command line cannot panic
/// the diagnostic that was supposed to explain what went wrong.
pub fn truncate_command(command: &str) -> String {
    let command = command.trim();
    if command.chars().count() <= MAX_ACTIVITY_COMMAND_CHARS {
        return command.to_string();
    }
    let mut truncated: String = command.chars().take(MAX_ACTIVITY_COMMAND_CHARS).collect();
    truncated.push('…');
    truncated
}

/// Select the descendant that best answers "what is this tree working on?".
///
/// Selection prefers the *longest-running spawned work* (depth >= 2) over the
/// direct child itself, breaking ties toward the deeper process. That is what
/// makes a six-minute build command — spawned by an agent that has also been up
/// six minutes — win over both the agent and the three-second compiler process
/// the build just launched. When the tree has no grandchildren the direct child
/// is reported instead, so a quiet agent still shows up.
///
/// `ignore_pids` exists because the probe's own `ps` process is a direct child
/// of the owner and would otherwise be a candidate.
pub fn select_descendant_activity(
    rows: &[ProcessActivityRow],
    owner_pid: u32,
    ignore_pids: &[u32],
) -> Option<DescendantActivity> {
    let descendants: Vec<(&ProcessActivityRow, usize)> = descendants_with_depth(rows, owner_pid)
        .into_iter()
        .filter(|(row, _)| !ignore_pids.contains(&row.pid))
        .collect();
    let descendant_count = descendants.len();
    let candidate = descendants
        .iter()
        .filter(|(row, _)| !row.command.is_empty());
    let spawned_work = candidate
        .clone()
        .filter(|(_, depth)| *depth >= 2)
        .max_by_key(|(row, depth)| (row.elapsed_seconds, *depth, row.pid));
    let selected =
        spawned_work.or_else(|| candidate.max_by_key(|(row, depth)| (*depth, row.elapsed_seconds)));
    let (row, depth) = selected?;
    Some(DescendantActivity {
        pid: row.pid,
        depth: *depth,
        elapsed_seconds: row.elapsed_seconds,
        command: truncate_command(&row.command),
        descendant_count,
    })
}

/// Walk the owner's descendants, recording hop depth for each.
fn descendants_with_depth(
    rows: &[ProcessActivityRow],
    owner_pid: u32,
) -> Vec<(&ProcessActivityRow, usize)> {
    let mut found: Vec<(&ProcessActivityRow, usize)> = Vec::new();
    let mut frontier = vec![(owner_pid, 0usize)];
    while let Some((parent, depth)) = frontier.pop() {
        // A pid cycle cannot exist in a well-formed process table, but a
        // torn `ps` snapshot can still produce one. Bound the walk by depth so
        // a malformed sample cannot spin the heartbeat thread.
        if depth >= 16 {
            continue;
        }
        for row in rows {
            if row.ppid != parent || row.pid == parent {
                continue;
            }
            if found.iter().any(|(seen, _)| seen.pid == row.pid) {
                continue;
            }
            found.push((row, depth + 1));
            frontier.push((row.pid, depth + 1));
        }
    }
    found
}

/// Sample the current activity of `owner_pid`'s process tree.
///
/// Returns `None` when the platform has no `ps`, the sample fails, or the tree
/// has no observable descendants. Activity is a diagnostic: a failed sample
/// must never be an error a caller has to handle.
pub fn descendant_activity(owner_pid: u32) -> Option<DescendantActivity> {
    #[cfg(unix)]
    {
        let output = Command::new("ps")
            .args(["-axo", "pid=,ppid=,etime=,args="])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let rows = parse_process_activity_rows(&stdout);
        // The probe's own `ps` is a direct child of the owner whenever the
        // owner is this process; never report the observation as the activity.
        select_descendant_activity(&rows, owner_pid, &[std::process::id()])
    }

    #[cfg(not(unix))]
    {
        let _ = owner_pid;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ps_etime_in_every_documented_shape() {
        assert_eq!(parse_ps_elapsed_seconds("05"), Some(5));
        assert_eq!(parse_ps_elapsed_seconds("06:12"), Some(372));
        assert_eq!(parse_ps_elapsed_seconds("01:06:12"), Some(3_972));
        assert_eq!(parse_ps_elapsed_seconds("2-01:06:12"), Some(176_772));
        assert_eq!(parse_ps_elapsed_seconds("not-a-time"), None);
        assert_eq!(parse_ps_elapsed_seconds(""), None);
    }

    #[test]
    fn parses_command_lines_that_contain_spaces() {
        let ps = "4242 100 06:12 timeout 1200 cargo test -q -p homeboy-agents\n";

        let rows = parse_process_activity_rows(ps);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 4242);
        assert_eq!(rows[0].ppid, 100);
        assert_eq!(rows[0].elapsed_seconds, 372);
        assert_eq!(
            rows[0].command,
            "timeout 1200 cargo test -q -p homeboy-agents"
        );
    }

    #[test]
    fn ignores_rows_that_are_not_process_table_entries() {
        let ps = "  PID  PPID     ELAPSED COMMAND\n4242 100 06:12 cargo test\n\n";

        let rows = parse_process_activity_rows(ps);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 4242);
    }

    #[test]
    fn selects_the_longest_running_spawned_work_over_the_provider_and_its_newest_child() {
        // The exact shape from #11482: a provider that has been up as long as
        // the compile it launched, plus a freshly started compiler child. The
        // actionable answer is `cargo test`, not `opencode` and not `rustc`.
        let ps = concat!(
            "100 1 06:20 opencode run --format json # Task: make Cook honor model rotation\n",
            "200 100 06:12 timeout 1200 cargo test -q -j3 -p homeboy-agents\n",
            "300 200 06:12 cargo test -q -j3 -p homeboy-agents\n",
            "400 300 00:03 rustc --crate-name homeboy_agents\n",
        );
        let rows = parse_process_activity_rows(ps);

        let activity = select_descendant_activity(&rows, 100, &[]).expect("activity is observable");

        assert_eq!(activity.pid, 300);
        assert_eq!(activity.depth, 2);
        assert_eq!(activity.elapsed_seconds, 372);
        assert!(activity.command.starts_with("cargo test"));
        assert_eq!(activity.descendant_count, 3);
    }

    #[test]
    fn reports_the_provider_itself_when_it_has_spawned_nothing() {
        // A provider that is thinking rather than shelling out still has to be
        // visible — "no descendants" must not read as "no activity".
        let ps = "100 1 02:00 opencode run --format json\n";
        let rows = parse_process_activity_rows(ps);

        let activity = select_descendant_activity(&rows, 1, &[]).expect("provider is observable");

        assert_eq!(activity.pid, 100);
        assert_eq!(activity.depth, 1);
        assert_eq!(activity.command, "opencode run --format json");
    }

    #[test]
    fn never_reports_the_observing_process_as_the_activity() {
        let ps = concat!(
            "100 1 06:20 opencode run\n",
            "999 1 00:00 ps -axo pid=,ppid=,etime=,args=\n",
        );
        let rows = parse_process_activity_rows(ps);

        let activity =
            select_descendant_activity(&rows, 1, &[999]).expect("provider is observable");

        assert_eq!(activity.pid, 100);
    }

    #[test]
    fn returns_nothing_when_the_tree_has_no_descendants() {
        let rows = parse_process_activity_rows("100 1 06:20 opencode run\n");

        assert_eq!(select_descendant_activity(&rows, 4242, &[]), None);
    }

    #[test]
    fn truncates_command_lines_on_character_boundaries() {
        let command = format!("cargo test {}", "é".repeat(400));

        let truncated = truncate_command(&command);

        assert_eq!(truncated.chars().count(), MAX_ACTIVITY_COMMAND_CHARS + 1);
        assert!(truncated.ends_with('…'));
        assert!(truncated.starts_with("cargo test"));
    }

    #[test]
    fn a_torn_process_table_cannot_spin_the_walk() {
        // A self-parenting row is impossible in a real table and trivial to
        // produce in a torn snapshot; the walk must still terminate.
        let ps = "100 100 01:00 wedged\n200 100 01:00 child\n";
        let rows = parse_process_activity_rows(ps);

        let activity = select_descendant_activity(&rows, 100, &[]).expect("child is observable");

        assert_eq!(activity.pid, 200);
    }
}
