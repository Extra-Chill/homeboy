//! Consumers observe settlement; only protocol adapters read the raw store.
#[test]
fn continuation_store_reads_stay_at_the_boundary() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = vec![
        root.join("homeboy-agents/src"),
        root.join("homeboy-cli/src/commands"),
    ];
    while let Some(path) = files.pop() {
        if path.is_dir() {
            if path.file_name().unwrap() != "tests" {
                files.extend(
                    std::fs::read_dir(path)
                        .unwrap()
                        .map(|entry| entry.unwrap().path()),
                );
            }
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap();
        if path.extension().is_none_or(|ext| ext != "rs") || name.contains("test") {
            continue;
        }
        let relative = path.strip_prefix(&root).unwrap().to_str().unwrap();
        // Protocol implementation, observation authority, and the two public
        // CLI protocol/status adapters explicitly excluded by #18531.
        if [
            "homeboy-agents/src/agent_task_service/cook_recipe.rs",
            "homeboy-agents/src/agent_task_service/cook_continuation.rs",
            "homeboy-agents/src/agent_task_lifecycle/cook_run_state.rs",
            "homeboy-cli/src/commands/agent_task/run_continuation.rs",
        ]
        .contains(&relative)
        {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let mut test = false;
        let mut opened = false;
        let mut depth = 0isize;
        for line in source.lines().map(str::trim) {
            if line == "#[cfg(test)]" || line == "#[test]" {
                test = true;
                opened = false;
                depth = 0;
                continue;
            }
            if test {
                opened |= line.contains('{');
                depth += line.matches('{').count() as isize - line.matches('}').count() as isize;
                if (opened && depth <= 0) || (!opened && line.ends_with(';')) {
                    test = false;
                }
                continue;
            }
            if !line.starts_with("//") {
                assert!(
                    !line.contains("continuation_state_in_store("),
                    "{relative} bypasses CookSettlement: {line}"
                );
            }
        }
    }
}
