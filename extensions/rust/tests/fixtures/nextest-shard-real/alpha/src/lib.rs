#[cfg(test)]
mod tests {
    #[test]
    fn selected_parent() {
        let binary = std::env::current_exe().expect("current test binary");
        let status = std::process::Command::new(&binary)
            .args(["--exact", "tests::ignored_child_helper"])
            .stdout(std::process::Stdio::inherit())
            .status()
            .expect("run ignored nested helper");
        assert!(status.success());
        assert!(std::process::Command::new(&binary)
            .args(["--exact", "tests::nested_ok_helper"])
            .stdout(std::process::Stdio::inherit())
            .status()
            .expect("run successful nested helper")
            .success());
        let _ = std::process::Command::new(&binary)
            .args(["--exact", "tests::nested_failed_helper"])
            .stdout(std::process::Stdio::inherit())
            .status()
            .expect("run failing nested helper");
    }

    #[test]
    #[ignore]
    fn planned_ignored() {
        assert!(true);
    }

    #[test]
    #[ignore]
    fn ignored_child_helper() {
        assert!(true);
    }

    #[test]
    fn nested_ok_helper() {
        assert!(true);
    }

    #[test]
    fn nested_failed_helper() {
        panic!("nested child failure is observational");
    }

    #[test]
    fn unrelated_lifecycle_event() {
        assert!(true);
    }
}
