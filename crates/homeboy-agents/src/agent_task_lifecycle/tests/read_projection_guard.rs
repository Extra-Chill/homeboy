//! Read surfaces must not grow a second retained-candidate classifier.

#[test]
fn read_surfaces_do_not_classify_candidate_labels() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let files = [
        "homeboy-cli/src/commands/agent_task/status.rs",
        "homeboy-cli/src/commands/cook_verdict.rs",
        "homeboy-cli/src/commands/agent_task_summary/mod.rs",
        "homeboy-agents/src/agent_task_notify.rs",
        "homeboy-agents/src/agent_task_notify_work.rs",
        "homeboy-agents/src/agent_task_notify_tracker_note.rs",
        "homeboy-agents/src/agent_task_scheduler/engine.rs",
        "homeboy-agents/src/agent_task_scheduler/scheduling.rs",
        "homeboy-agents/src/agent_task_schedule.rs",
    ];
    for file in files {
        let source = std::fs::read_to_string(root.join("crates").join(file)).unwrap();
        // Remove inline test items, not the remainder of the file: status has
        // production functions following several inline test modules.
        let mut test_item = false;
        let mut opened = false;
        let mut depth = 0isize;
        for (line, text) in source.lines().enumerate() {
            let text = text.trim();
            if text == "#[cfg(test)]" || text == "#[test]" {
                test_item = true;
                opened = false;
                depth = 0;
                continue;
            }
            if test_item {
                opened |= text.contains('{');
                depth += text.matches('{').count() as isize - text.matches('}').count() as isize;
                if (opened && depth <= 0) || (!opened && text.ends_with(';')) {
                    test_item = false;
                }
                continue;
            }
            if text.starts_with("//") {
                continue;
            }
            assert!(
                !text.contains("let candidate_recoverable")
                    && !(text.contains("candidate_recoverable")
                        && (text.contains("==") || text.contains("!=") || text.starts_with("if "))),
                "{file}:{} reclassifies a Cook candidate: {text}",
                line + 1
            );
        }
    }
}
