//! Read-only process-tree activity sampling.
//!
//! [`homeboy_core::process`] owns the *lifecycle* half of the process tree (signalling,
//! scopes, termination). This module owns the *observability* half: answering
//! "what is this process tree actually doing right now?" without changing it.
//!
//! The motivating case is diagnosing a stalled Cook. Before this existed the
//! only way to learn that a provider agent was compiling instead of editing was
//! `ps aux | grep` on the host (#11482). The same `ps -axo` walk that
//! [`homeboy_core::process`] already uses for descendant discovery answers it, so the
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

/// Why a sampled tree carries no provider activity.
///
/// Reported instead of a command because #11598 showed the failure mode of the
/// alternative: a heartbeat that named Homeboy's own `agent-task cook` process
/// for twenty minutes read as an authoritative statement about the provider
/// while the provider was doing nothing at all. "We looked and found no
/// provider process" is a weaker claim, and it is the true one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityUnavailable {
    /// The owner has no observable descendants.
    NoDescendants,
    /// Every descendant is Homeboy's own machinery — a nested controller
    /// process, a gate, or a CLI call the agent made — so none of them answers
    /// "what is the provider doing right now".
    OnlyHomeboyProcesses,
}

impl ActivityUnavailable {
    /// A bounded operator-facing reason string.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NoDescendants => "no provider process observed under the cook",
            Self::OnlyHomeboyProcesses => "only homeboy's own processes observed under the cook",
        }
    }
}

/// The result of one selection pass over a process table.
///
/// Carries three distinguishable states, because collapsing them is what made
/// the original signal misleading:
///
/// - `activity: Some(_)` — a provider process was observed.
/// - `activity: None`, `unavailable: Some(_)` — the tree was sampled and
///   nothing in it qualified.
/// - `activity: None`, `unavailable: None` — no sample was taken at all
///   (no `ps`, a failed probe, a non-Unix host).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderActivitySample {
    pub activity: Option<DescendantActivity>,
    /// Descendants observed under the owner, Homeboy's own processes included.
    pub descendant_count: usize,
    pub unavailable: Option<ActivityUnavailable>,
}

impl ProviderActivitySample {
    /// The state for "we never looked", which must not be dressed up as a
    /// measurement of an idle tree.
    pub fn unsampled() -> Self {
        Self::default()
    }
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

/// Select the descendant that best answers "what is the *provider* working on?".
///
/// The original heuristic (#11492) preferred spawned work at depth >= 2 and
/// fell back to the deepest direct child. It read the question as a shape
/// question — "how far down the tree is this?" — and the shape does not
/// identify the process. A locally placed Cook re-enters Homeboy as a *child*
/// of the observing process, so the cook's own command line sits at depth 1
/// with the age of the whole run and won every fallback: #11598 saw
/// `homeboy agent-task cook --placement local --wait …` reported as provider
/// activity for twenty minutes while zero files were written. Depth arithmetic
/// cannot exclude that, because the offending process is genuinely a
/// descendant.
///
/// So identity is now the filter and age is only the ranking:
///
/// 1. Exclude the walk root and its ancestry outright, rather than trusting
///    depth to keep them out of the candidate set.
/// 2. Exclude every Homeboy-owned process. A nested `agent-task cook`, a gate,
///    and the `contract constants all` call an agent made as a tool are all
///    Homeboy's own orchestration; none of them is the provider's work, and
///    the short-lived ones were winning the early heartbeats.
/// 3. Among what remains, take the longest-running process, breaking ties
///    toward the deeper one. A six-minute build spawned by an agent that has
///    also been up six minutes still beats both the agent and the three-second
///    compiler the build just launched.
/// 4. If nothing remains, report [`ActivityUnavailable`] — never a Homeboy
///    command relabelled as provider activity.
///
/// `ignore_pids` exists because the probe's own `ps` process is a direct child
/// of the owner and would otherwise be a candidate.
pub fn select_provider_activity(
    rows: &[ProcessActivityRow],
    owner_pid: u32,
    ignore_pids: &[u32],
) -> ProviderActivitySample {
    let excluded = excluded_pids(rows, owner_pid, ignore_pids);
    let descendants: Vec<(&ProcessActivityRow, usize)> = descendants_with_depth(rows, owner_pid)
        .into_iter()
        .filter(|(row, _)| !excluded.contains(&row.pid))
        .collect();
    let descendant_count = descendants.len();
    let selected = descendants
        .iter()
        .filter(|(row, _)| !row.command.is_empty())
        .filter(|(row, _)| !is_homeboy_process(&row.command))
        .max_by_key(|(row, depth)| (row.elapsed_seconds, *depth, row.pid));
    match selected {
        Some((row, depth)) => ProviderActivitySample {
            activity: Some(DescendantActivity {
                pid: row.pid,
                depth: *depth,
                elapsed_seconds: row.elapsed_seconds,
                command: truncate_command(&row.command),
                descendant_count,
            }),
            descendant_count,
            unavailable: None,
        },
        None => ProviderActivitySample {
            activity: None,
            descendant_count,
            unavailable: Some(if descendant_count == 0 {
                ActivityUnavailable::NoDescendants
            } else {
                ActivityUnavailable::OnlyHomeboyProcesses
            }),
        },
    }
}

/// Pids that can never be reported: the walk root, everything above it, and
/// whatever the caller asked to ignore.
///
/// A well-formed process table cannot present an ancestor as a descendant, so
/// this is belt-and-braces against a torn snapshot — but it is also the point:
/// the rule "never report the cook or anything that spawned it" is stated
/// directly instead of being an emergent property of a depth comparison.
fn excluded_pids(rows: &[ProcessActivityRow], owner_pid: u32, ignore_pids: &[u32]) -> Vec<u32> {
    let mut excluded = ignore_pids.to_vec();
    excluded.push(owner_pid);
    let mut current = owner_pid;
    for _ in 0..MAX_PROCESS_WALK_DEPTH {
        let Some(row) = rows.iter().find(|row| row.pid == current) else {
            break;
        };
        if row.ppid == 0 || excluded.contains(&row.ppid) {
            break;
        }
        excluded.push(row.ppid);
        current = row.ppid;
    }
    excluded
}

/// Maximum hops taken when walking the process table in either direction.
///
/// A pid cycle cannot exist in a well-formed table, but a torn `ps` snapshot
/// can still produce one, and the heartbeat thread must not spin on it.
const MAX_PROCESS_WALK_DEPTH: usize = 16;

/// Wrapper programs that front a real command, so the program that matters is
/// a later token.
///
/// Deliberately short: guessing wrong here means misreading the program name,
/// and the provider's own command line embeds the whole task prompt — which is
/// exactly why this looks at the program token and never at the command line
/// as a whole. A prompt that mentions Homeboy must not make the provider look
/// like Homeboy.
const COMMAND_WRAPPERS: &[&str] = &[
    "env", "sh", "bash", "zsh", "dash", "timeout", "nice", "setsid", "stdbuf", "nohup", "time",
];

/// True when the sampled command line is one of Homeboy's own processes.
pub fn is_homeboy_process(command: &str) -> bool {
    let program = command_program(command);
    program == "homeboy" || program.starts_with("homeboy-") || program.starts_with("homeboy.")
}

/// The program a command line actually runs, as a bare file name.
///
/// Skips a bounded number of leading wrapper tokens (`timeout 1200 cargo …`,
/// `sh -c homeboy …`) and their flag/numeric arguments so a wrapped Homeboy
/// invocation is still recognized as Homeboy.
fn command_program(command: &str) -> &str {
    let mut tokens = command.split_whitespace();
    let mut program = "";
    for _ in 0..8 {
        let Some(token) = tokens.next() else { break };
        // Flags, `KEY=value` env prefixes and bare numeric arguments belong to
        // the wrapper that preceded them, not to the program being run.
        if token.starts_with('-') || token.contains('=') || token.parse::<u64>().is_ok() {
            continue;
        }
        program = base_name(token);
        if !COMMAND_WRAPPERS.contains(&program) {
            break;
        }
    }
    program
}

/// The final path segment of a token, for both `/` and `\` separated paths.
fn base_name(token: &str) -> &str {
    token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .trim_matches('"')
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
        if depth >= MAX_PROCESS_WALK_DEPTH {
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
/// Returns [`ProviderActivitySample::unsampled`] when the platform has no `ps`
/// or the probe fails — distinct from a successful sample that found no
/// provider work, because "we could not look" and "we looked and the provider
/// is not running anything" are different facts about a stalled cook. Activity
/// is a diagnostic: a failed sample must never be an error a caller has to
/// handle.
pub fn descendant_activity(owner_pid: u32) -> ProviderActivitySample {
    #[cfg(unix)]
    {
        let Some(output) = Command::new("ps")
            .args(["-axo", "pid=,ppid=,etime=,args="])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
        else {
            return ProviderActivitySample::unsampled();
        };
        if !output.status.success() {
            return ProviderActivitySample::unsampled();
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let rows = parse_process_activity_rows(&stdout);
        // The probe's own `ps` is a direct child of the owner whenever the
        // owner is this process; never report the observation as the activity.
        select_provider_activity(&rows, owner_pid, &[std::process::id()])
    }

    #[cfg(not(unix))]
    {
        let _ = owner_pid;
        ProviderActivitySample::unsampled()
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

        let activity = select_provider_activity(&rows, 100, &[])
            .activity
            .expect("activity is observable");

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

        let activity = select_provider_activity(&rows, 1, &[])
            .activity
            .expect("provider is observable");

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

        let activity = select_provider_activity(&rows, 1, &[999])
            .activity
            .expect("provider is observable");

        assert_eq!(activity.pid, 100);
    }

    #[test]
    fn returns_nothing_when_the_tree_has_no_descendants() {
        let rows = parse_process_activity_rows("100 1 06:20 opencode run\n");

        let sample = select_provider_activity(&rows, 4242, &[]);

        assert_eq!(sample.activity, None);
        assert_eq!(sample.descendant_count, 0);
        assert_eq!(sample.unavailable, Some(ActivityUnavailable::NoDescendants));
    }

    #[test]
    fn never_reports_the_cook_controller_that_homeboy_re_entered_locally() {
        // The #11598 regression, verbatim. A locally placed cook re-enters
        // Homeboy, so the observing controller has a child running the same
        // `agent-task cook` argv with the age of the whole run. Depth put it at
        // 1 and age put it first, so it was reported as provider activity for
        // twenty minutes while nothing was written.
        let ps = concat!(
            "100 1 20:16 /usr/local/bin/homeboy agent-task cook --placement local --wait\n",
            "200 100 20:14 /usr/local/bin/homeboy agent-task cook --placement local --wait\n",
            "300 200 20:10 opencode run --format json # Task: fix homeboy cook attribution\n",
        );
        let rows = parse_process_activity_rows(ps);

        let sample = select_provider_activity(&rows, 100, &[]);

        let activity = sample.activity.expect("the provider is observable");
        assert_eq!(activity.pid, 300);
        assert!(activity.command.starts_with("opencode run"));
        assert_eq!(sample.unavailable, None);
    }

    #[test]
    fn a_tree_of_only_homeboy_processes_reports_no_activity_rather_than_the_controller() {
        // Same shape with the provider gone: the honest answer is that no
        // provider process was observed. Naming the nested cook here is what
        // made the signal look authoritative while being wrong.
        let ps = concat!(
            "100 1 20:16 /usr/local/bin/homeboy agent-task cook --placement local --wait\n",
            "200 100 20:14 /usr/local/bin/homeboy agent-task cook --placement local --wait\n",
            "300 200 00:13 homeboy contract constants all\n",
        );
        let rows = parse_process_activity_rows(ps);

        let sample = select_provider_activity(&rows, 100, &[]);

        assert_eq!(sample.activity, None);
        assert_eq!(sample.descendant_count, 2);
        assert_eq!(
            sample.unavailable,
            Some(ActivityUnavailable::OnlyHomeboyProcesses)
        );
    }

    #[test]
    fn a_short_lived_homeboy_tool_call_never_outranks_the_provider() {
        // The early heartbeats in #11598 reported `homeboy contract constants
        // all`, thirteen seconds old, because it was the deepest process in the
        // tree. Tool calls the agent makes into Homeboy are Homeboy's work, not
        // the provider's.
        let ps = concat!(
            "100 1 06:20 opencode run --format json\n",
            "200 100 00:13 homeboy contract constants all\n",
        );
        let rows = parse_process_activity_rows(ps);

        let activity = select_provider_activity(&rows, 1, &[])
            .activity
            .expect("the provider is observable");

        assert_eq!(activity.pid, 100);
        assert!(activity.command.starts_with("opencode run"));
    }

    #[test]
    fn an_ancestor_of_the_walk_root_is_never_reported() {
        // A torn snapshot can present a cycle in which an ancestor looks like a
        // descendant. The exclusion is stated on identity, so it holds anyway.
        let ps = concat!(
            "100 200 30:00 /usr/local/bin/homeboy agent-task cook --wait\n",
            "200 100 29:00 opencode run --format json\n",
        );
        let rows = parse_process_activity_rows(ps);

        let sample = select_provider_activity(&rows, 200, &[]);

        assert_eq!(sample.activity, None);
        assert_eq!(sample.unavailable, Some(ActivityUnavailable::NoDescendants));
    }

    #[test]
    fn homeboy_is_recognized_however_it_was_invoked_and_never_from_a_prompt() {
        // A provider command line embeds the whole task prompt, so anything
        // that matched on the command line as a whole would classify the
        // provider itself as Homeboy and report nothing at all.
        assert!(is_homeboy_process("homeboy agent-task cook --wait"));
        assert!(is_homeboy_process("/usr/local/bin/homeboy review lint"));
        assert!(is_homeboy_process("sh -c homeboy contract constants all"));
        assert!(is_homeboy_process("timeout 600 homeboy agent-task status"));
        assert!(is_homeboy_process("homeboy-lab-runner exec"));
        assert!(!is_homeboy_process(
            "opencode run --format json # Task: fix homeboy agent-task cook"
        ));
        assert!(!is_homeboy_process(
            "timeout 1200 cargo test -p homeboy-agents"
        ));
        assert!(!is_homeboy_process(""));
    }

    #[test]
    fn a_probe_that_could_not_look_is_not_a_measurement() {
        // "No sample" and "sampled, found no provider" are different facts and
        // must stay distinguishable downstream.
        let unsampled = ProviderActivitySample::unsampled();

        assert_eq!(unsampled.activity, None);
        assert_eq!(unsampled.unavailable, None);
        assert_eq!(unsampled.descendant_count, 0);
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

        let activity = select_provider_activity(&rows, 100, &[])
            .activity
            .expect("child is observable");

        assert_eq!(activity.pid, 200);
    }
}
