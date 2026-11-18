use homeboy::core::test_support::{HermeticTestContext, TestBinary};

/// A controller mutation issued from a Cook attempt's workspace is refused
/// before it reaches the daemon, naming the attempt; the explicit flag lets
/// it through to the daemon.
#[cfg(target_os = "linux")]
#[test]
fn workflow_cancel_from_inside_a_cook_attempt_is_refused_unless_allowed() {
    let context = HermeticTestContext::new();
    let workspace = context
        .data_dir()
        .join("controller-scratch/attempts/attempt-under-test/workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    for (args, refused) in [
        (vec!["workflow", "cancel", "wf-missing"], true),
        (
            vec!["workflow", "cancel", "wf-missing", "--allow-inside-cook"],
            false,
        ),
    ] {
        let output = context
            .command(TestBinary::HomeboyFixture)
            .args(&args)
            .current_dir(&workspace)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success(), "{args:?}: {text}");
        assert_eq!(
            text.contains("runs inside Cook attempt `attempt-under-test`"),
            refused,
            "{args:?}: {text}"
        );
    }
}
