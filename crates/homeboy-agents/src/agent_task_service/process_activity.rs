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

use homeboy_engine_primitives::process_table;
#[cfg(test)]
use homeboy_engine_primitives::process_table::parse_snapshot;
use homeboy_engine_primitives::process_table::ProcessRow;

/// Maximum characters retained from a sampled command line.
///
/// Provider command lines embed the whole task prompt, so an untruncated
/// sample would push kilobytes into every heartbeat and durable progress
/// record. The retained prefix is where the actionable part lives — the program
/// and its leading arguments — so truncate rather than drop.
pub const MAX_ACTIVITY_COMMAND_CHARS: usize = 200;

/// The descendant selected as "what the tree is currently doing".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderProcessActivity {
    pub pid: u32,
    /// Hops from the observed owner. `1` is a direct child (for a Cook, the
    /// provider process itself); `2` and deeper is work the provider spawned.
    pub depth: usize,
    pub elapsed_seconds: u64,
    /// Resident set size of the selected process alone, in KiB, when observed.
    pub rss_kib: Option<u64>,
    /// Command line, truncated to [`MAX_ACTIVITY_COMMAND_CHARS`].
    pub command: String,
    /// Process-table evidence, rather than a controller lifecycle assertion.
    pub source: &'static str,
    /// The selected descendant is the executor itself or a tool it spawned.
    pub activity_type: &'static str,
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
    pub activity: Option<ProviderProcessActivity>,
    /// Descendants observed under the owner, Homeboy's own processes included.
    pub descendant_count: usize,
    /// Summed resident set size across every observed descendant, in KiB.
    ///
    /// Summing double-counts pages shared between a provider and the tools it
    /// spawned, so this is an upper bound on the tree's footprint rather than a
    /// precise figure. It is the number an operator watching a box run out of
    /// memory actually reasons about, and an upper bound is the safe side for a
    /// budget. `None` when no sampled row carried an `rss` column at all —
    /// never `0`, which would read as "this tree holds nothing".
    pub tree_rss_kib: Option<u64>,
    pub unavailable: Option<ActivityUnavailable>,
}

impl ProviderActivitySample {
    /// The state for "we never looked", which must not be dressed up as a
    /// measurement of an idle tree.
    pub fn unsampled() -> Self {
        Self::default()
    }
}

fn is_defunct(row: &ProcessRow) -> bool {
    row.state
        .as_deref()
        .is_some_and(|state| state.starts_with('Z') || state.starts_with('X'))
}

/// Truncate a command line to [`MAX_ACTIVITY_COMMAND_CHARS`] characters.
///
/// Truncation is on `char` boundaries so a multi-byte command line cannot panic
/// the diagnostic that was supposed to explain what went wrong.
pub fn truncate_command(command: &str) -> String {
    homeboy_engine_primitives::text::truncate_chars(command.trim(), MAX_ACTIVITY_COMMAND_CHARS, "…")
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
    rows: &[ProcessRow],
    owner_pid: u32,
    ignore_pids: &[u32],
) -> ProviderActivitySample {
    let excluded = excluded_pids(rows, owner_pid, ignore_pids);
    let descendants: Vec<(&ProcessRow, usize)> = descendants_with_depth(rows, owner_pid)
        .into_iter()
        .filter(|(row, _)| !excluded.contains(&row.pid))
        .collect();
    // A zombie can retain live descendants in the sampled tree, so it remains
    // in the graph traversal but never becomes the reported activity.
    let live_descendants: Vec<(&ProcessRow, usize)> = descendants
        .into_iter()
        .filter(|(row, _)| !is_defunct(row))
        .collect();
    let descendant_count = live_descendants.len();
    // Footprint is a property of the whole tree, not of the one process that
    // best answers "what is it doing": a provider that spawned four linkers is
    // holding what the linkers hold. Homeboy's own descendants are counted too
    // — they are still occupying this host's memory.
    let tree_rss_kib = live_descendants
        .iter()
        .filter_map(|(row, _)| row.rss_kib)
        .reduce(u64::saturating_add);
    let selected = live_descendants
        .iter()
        .filter(|(row, _)| !row.command.is_empty())
        .filter(|(row, _)| !is_homeboy_process(&row.command))
        .max_by_key(|(row, depth)| (row.elapsed_seconds, *depth, row.pid));
    match selected {
        Some((row, depth)) => ProviderActivitySample {
            activity: Some(ProviderProcessActivity {
                pid: row.pid,
                depth: *depth,
                elapsed_seconds: row.elapsed_seconds,
                rss_kib: row.rss_kib,
                command: truncate_command(&row.command),
                source: "process_tree",
                activity_type: if *depth == 1 { "executor" } else { "tool" },
                descendant_count,
            }),
            descendant_count,
            tree_rss_kib,
            unavailable: None,
        },
        None => ProviderActivitySample {
            activity: None,
            descendant_count,
            tree_rss_kib,
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
fn excluded_pids(rows: &[ProcessRow], owner_pid: u32, ignore_pids: &[u32]) -> Vec<u32> {
    let mut excluded = ignore_pids.to_vec();
    excluded.push(owner_pid);
    excluded.extend(
        process_table::ancestors(owner_pid, rows.iter().map(|row| (row.pid, row.parent_pid)))
            .into_iter()
            .take(MAX_PROCESS_WALK_DEPTH),
    );
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
fn descendants_with_depth(rows: &[ProcessRow], owner_pid: u32) -> Vec<(&ProcessRow, usize)> {
    process_table::descendants(owner_pid, rows.iter().map(|row| (row.pid, row.parent_pid)))
        .into_iter()
        .filter(|(_, depth)| *depth <= MAX_PROCESS_WALK_DEPTH)
        .filter_map(|(pid, depth)| {
            rows.iter()
                .find(|row| row.pid == pid)
                .map(|row| (row, depth))
        })
        .collect()
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
        let Ok(snapshot) = process_table::snapshot(false) else {
            return ProviderActivitySample::unsampled();
        };
        let rows = snapshot;
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
    fn a_defunct_root_is_not_reported_when_its_descendants_are_live() {
        // A reaped provider launcher can remain as a zombie while its children
        // finish work. Keep it as a graph bridge, but report the live command
        // and count only processes that can still consume host resources.
        let ps = concat!(
            "100 1 Z 0 0 02:54 1000 Wed Nov 5 12:00:00 2026 tool <defunct>\n",
            "200 100 S 524288 0 02:53 1000 Wed Nov 5 12:00:00 2026 tool pi run --format json\n",
            "300 200 R 262144 0 00:12 1000 Wed Nov 5 12:00:00 2026 tool cargo test -q -p homeboy-agents\n",
        );
        let rows = parse_snapshot(ps);

        let sample = select_provider_activity(&rows, 1, &[]);

        let activity = sample.activity.expect("live descendant is observable");
        assert_eq!(activity.pid, 200);
        assert_eq!(activity.command, "pi run --format json");
        assert_eq!(activity.descendant_count, 2);
        assert_eq!(sample.descendant_count, 2);
        assert_eq!(sample.tree_rss_kib, Some(786_432));
    }

    #[test]
    fn tree_memory_is_summed_across_every_observed_descendant() {
        // The supervision question is what the whole tree is holding on this
        // host, so a provider plus the compilers it launched is one number.
        let ps = concat!(
            "100 1 S 262144 0 06:20 1000 Wed Nov 5 12:00:00 2026 tool pi run --format json\n",
            "200 100 S 524288 0 06:12 1000 Wed Nov 5 12:00:00 2026 tool cargo test -q\n",
            "300 200 S 1048576 0 00:03 1000 Wed Nov 5 12:00:00 2026 tool rustc --crate-name homeboy_agents\n",
        );
        let rows = parse_snapshot(ps);

        let sample = select_provider_activity(&rows, 1, &[]);

        assert_eq!(sample.tree_rss_kib, Some(1_835_008));
        assert_eq!(sample.descendant_count, 3);
        let activity = sample.activity.expect("provider is observable");
        // Selection is longest-running non-homeboy descendant (#11598 removed
        // the depth preference that reported the controller). `pi run`
        // has been alive longest, so it is what is reported — and it carries
        // only its own footprint, which is why the tree sum above exists.
        assert_eq!(activity.command, "pi run --format json");
        assert_eq!(activity.rss_kib, Some(262_144));
    }

    #[test]
    fn concurrent_provider_roots_do_not_cross_attribute_activity_or_resources() {
        // Both provider children share a coordinator parent. Sampling from that
        // parent would blend their commands and resource trees; each child PID
        // must be used as the root instead.
        let rows = parse_snapshot(concat!(
            "1 0 S 1024 0 10:00 1000 Wed Nov 5 12:00:00 2026 tool homeboy agent-task fanout cook\n",
            "101 1 S 2048 0 05:00 1000 Wed Nov 5 12:00:00 2026 tool pi run child-a\n",
            "102 101 S 4096 0 04:00 1000 Wed Nov 5 12:00:00 2026 tool cargo test child-a\n",
            "201 1 S 8192 0 05:00 1000 Wed Nov 5 12:00:00 2026 tool pi run child-b\n",
            "202 201 S 16384 0 04:00 1000 Wed Nov 5 12:00:00 2026 tool cargo test child-b\n",
        ));

        let first = select_provider_activity(&rows, 101, &[]);
        let second = select_provider_activity(&rows, 201, &[]);

        assert_eq!(
            first.activity.expect("first activity").command,
            "cargo test child-a"
        );
        assert_eq!(
            second.activity.expect("second activity").command,
            "cargo test child-b"
        );
        assert_eq!(first.tree_rss_kib, Some(4_096));
        assert_eq!(second.tree_rss_kib, Some(16_384));
    }

    #[test]
    fn an_unmeasured_tree_reports_no_memory_rather_than_zero() {
        // A budget that fired on a fabricated zero would be worse than no
        // budget at all, so the absence has to survive selection.
        let ps = "100 1 S unknown 0 06:20 1000 Wed Nov 5 12:00:00 2026 tool pi run\n";
        let rows = parse_snapshot(ps);

        assert_eq!(select_provider_activity(&rows, 1, &[]).tree_rss_kib, None);
    }

    #[test]
    fn selects_the_longest_running_spawned_work_over_the_provider_and_its_newest_child() {
        // The exact shape from #11482: a provider that has been up as long as
        // the compile it launched, plus a freshly started compiler child. The
        // actionable answer is `cargo test`, not `pi` and not `rustc`.
        let ps = concat!(
            "100 1 S unknown 0 06:20 1000 Wed Nov 5 12:00:00 2026 tool pi run --format json # Task: make Cook honor model rotation\n",
            "200 100 S unknown 0 06:12 1000 Wed Nov 5 12:00:00 2026 tool timeout 1200 cargo test -q -j3 -p homeboy-agents\n",
            "300 200 S unknown 0 06:12 1000 Wed Nov 5 12:00:00 2026 tool cargo test -q -j3 -p homeboy-agents\n",
            "400 300 S unknown 0 00:03 1000 Wed Nov 5 12:00:00 2026 tool rustc --crate-name homeboy_agents\n",
        );
        let rows = parse_snapshot(ps);

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
        let ps = "100 1 S unknown 0 02:00 1000 Wed Nov 5 12:00:00 2026 tool pi run --format json\n";
        let rows = parse_snapshot(ps);

        let activity = select_provider_activity(&rows, 1, &[])
            .activity
            .expect("provider is observable");

        assert_eq!(activity.pid, 100);
        assert_eq!(activity.depth, 1);
        assert_eq!(activity.command, "pi run --format json");
    }

    #[test]
    fn never_reports_the_observing_process_as_the_activity() {
        let ps = concat!(
            "100 1 S unknown 0 06:20 1000 Wed Nov 5 12:00:00 2026 tool pi run\n",
            "999 1 S unknown 0 00:00 1000 Wed Nov 5 12:00:00 2026 tool ps -axo pid=,ppid=,etime=,args=\n",
        );
        let rows = parse_snapshot(ps);

        let activity = select_provider_activity(&rows, 1, &[999])
            .activity
            .expect("provider is observable");

        assert_eq!(activity.pid, 100);
    }

    #[test]
    fn returns_nothing_when_the_tree_has_no_descendants() {
        let rows =
            parse_snapshot("100 1 S unknown 0 06:20 1000 Wed Nov 5 12:00:00 2026 tool pi run\n");

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
            "100 1 S unknown 0 20:16 1000 Wed Nov 5 12:00:00 2026 tool /usr/local/bin/homeboy agent-task cook --placement local --wait\n",
            "200 100 S unknown 0 20:14 1000 Wed Nov 5 12:00:00 2026 tool /usr/local/bin/homeboy agent-task cook --placement local --wait\n",
            "300 200 S unknown 0 20:10 1000 Wed Nov 5 12:00:00 2026 tool pi run --format json # Task: fix homeboy cook attribution\n",
        );
        let rows = parse_snapshot(ps);

        let sample = select_provider_activity(&rows, 100, &[]);

        let activity = sample.activity.expect("the provider is observable");
        assert_eq!(activity.pid, 300);
        assert!(activity.command.starts_with("pi run"));
        assert_eq!(sample.unavailable, None);
    }

    #[test]
    fn a_tree_of_only_homeboy_processes_reports_no_activity_rather_than_the_controller() {
        // Same shape with the provider gone: the honest answer is that no
        // provider process was observed. Naming the nested cook here is what
        // made the signal look authoritative while being wrong.
        let ps = concat!(
            "100 1 S unknown 0 20:16 1000 Wed Nov 5 12:00:00 2026 tool /usr/local/bin/homeboy agent-task cook --placement local --wait\n",
            "200 100 S unknown 0 20:14 1000 Wed Nov 5 12:00:00 2026 tool /usr/local/bin/homeboy agent-task cook --placement local --wait\n",
            "300 200 S unknown 0 00:13 1000 Wed Nov 5 12:00:00 2026 tool homeboy contract constants all\n",
        );
        let rows = parse_snapshot(ps);

        let sample = select_provider_activity(&rows, 100, &[]);

        assert_eq!(sample.activity, None);
        assert_eq!(sample.descendant_count, 2);
        assert_eq!(
            sample.unavailable,
            Some(ActivityUnavailable::OnlyHomeboyProcesses)
        );
    }

    #[test]
    fn an_ancestor_of_the_walk_root_is_never_reported() {
        // A torn snapshot can present a cycle in which an ancestor looks like a
        // descendant. The exclusion is stated on identity, so it holds anyway.
        let ps = concat!(
            "100 200 S unknown 0 30:00 1000 Wed Nov 5 12:00:00 2026 tool /usr/local/bin/homeboy agent-task cook --wait\n",
            "200 100 S unknown 0 29:00 1000 Wed Nov 5 12:00:00 2026 tool pi run --format json\n",
        );
        let rows = parse_snapshot(ps);

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
            "pi run --format json # Task: fix homeboy agent-task cook"
        ));
        assert!(!is_homeboy_process(
            "timeout 1200 cargo test -p homeboy-agents"
        ));
        assert!(!is_homeboy_process(""));
    }
}
