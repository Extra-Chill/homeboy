#[cfg(test)]
mod tests {
    #[test]
    fn selected_parent() {
        let status =
            std::process::Command::new(std::env::current_exe().expect("current test binary"))
                .args(["--exact", "tests::ignored_child_helper"])
                .stdout(std::process::Stdio::inherit())
                .status()
                .expect("run ignored nested helper");
        assert!(status.success());
    }

    #[test]
    #[ignore]
    fn ignored_child_helper() {
        assert!(true);
    }

    #[test]
    fn unrelated_lifecycle_event() {
        assert!(true);
    }
}
