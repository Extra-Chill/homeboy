use super::*;

#[test]
fn cancelling_either_runner_call_reaps_its_child_scope() {
    crate::test_support::with_isolated_home(|_| {
        let fixture = tempfile::tempdir().unwrap();
        let bin = fixture.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let accepted = fixture.path().join("accepted");
        let descendant = fixture.path().join("pid");
        homeboy_test_support::write_executable(&bin.join("ssh"), &format!(
            "#!/bin/sh\nsleep 60 & child=$!\nprintf '%s' \"$child\" > {}\nprintf accepted > {}\nwait\n",
            shell::quote_arg(&descendant.display().to_string()), shell::quote_arg(&accepted.display().to_string()),
        ), 0o755).unwrap();
        homeboy_core::server::create(
            r#"{"id":"protocol-transport","host":"materialization.invalid","user":"test"}"#,
            false,
        )
        .unwrap();
        crate::create(&serde_json::json!({"id":"protocol-transport","kind":"ssh","server_id":"protocol-transport","workspace_root":fixture.path(),"homeboy_path":"/runner/homeboy"}).to_string(), false).unwrap();
        let runner = load("protocol-transport").unwrap();
        fs::write(fixture.path().join("plan.json"), b"{}").unwrap();
        let old_path = std::env::var("PATH").unwrap();
        std::env::set_var("PATH", format!("{}:{old_path}", bin.display()));
        for commit in [false, true] {
            let marker = accepted.clone();
            let control =
                WorkspaceControl::new(homeboy_core::cooperative_control::CooperativeControl::new(
                    Instant::now() + Duration::from_secs(10),
                    std::sync::Arc::new(move || marker.exists()),
                ));
            let error =
                protocol_controller::invoke(&runner, fixture.path(), commit, &control).unwrap_err();
            assert_eq!(error.details["workspace_sync"]["cancelled"], true);
            if commit {
                assert_eq!(
                    error.details["workspace_sync"]["remote_effect_uncertain"],
                    true
                );
            }
            let pid = fs::read_to_string(&descendant)
                .unwrap()
                .parse::<u32>()
                .unwrap();
            assert!(
                !homeboy_core::process::pid_is_running(pid),
                "runner child leaked on cancellation"
            );
            fs::remove_file(&accepted).unwrap();
        }
        std::env::set_var("PATH", old_path);
    });
}
