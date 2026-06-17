use super::*;
use homeboy::core::error::ErrorCode;
use std::fs;

fn set_rig_resources(home: &tempfile::TempDir, rig_id: &str, resources: serde_json::Value) {
    let rig_path = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("rigs")
        .join(format!("{}.json", rig_id));
    let mut rig_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&rig_path).expect("read rig")).expect("parse rig");
    rig_json["resources"] = resources;
    fs::write(
        &rig_path,
        serde_json::to_string(&rig_json).expect("serialize rig"),
    )
    .expect("write rig");
}

#[test]
fn run_single_rig_bench_fails_fast_on_active_resource_lease() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let active_component = tempfile::TempDir::new().expect("active component");
        let blocked_component = tempfile::TempDir::new().expect("blocked component");
        write_rig(home, "studio", "studio", active_component.path());
        write_rig(home, "studio-bfb", "studio", blocked_component.path());
        let resources = serde_json::json!({
            "exclusive": ["studio-runtime"],
            "paths": ["~/Developer/studio"],
            "ports": [9724],
            "process_patterns": ["app-server-child.mjs"]
        });
        set_rig_resources(home, "studio", resources.clone());
        set_rig_resources(home, "studio-bfb", resources);

        let active_spec = homeboy::core::rig::load("studio").expect("load active rig");
        let _lease = homeboy::core::rig::lease::acquire_active_run_lease(&active_spec, "bench")
            .expect("acquire active lease")
            .expect("resourceful rig leases");

        let error = match run(
            run_args(None, vec!["studio-bfb".to_string()], Vec::new()),
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("bench should fail before running with conflicting rig resources"),
            Err(error) => error,
        };

        assert_eq!(error.code, ErrorCode::RigResourceConflict);
        assert!(error.message.contains("studio-bfb"));
        assert!(error.message.contains("studio"));
        assert!(error.message.contains("studio-runtime"));
        assert!(error.message.contains("bench"));
    });
}

#[test]
fn run_single_rig_bench_without_resources_does_not_create_lease() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig(home, "studio-lite", "studio", component_dir.path());

        let (_output, exit_code) = run(
            run_args(None, vec!["studio-lite".to_string()], Vec::new()),
            &GlobalArgs {},
        )
        .expect("non-resourceful rig bench should run");

        assert_eq!(exit_code, 0);
        assert!(homeboy::core::rig::lease::active_run_leases()
            .expect("list active leases")
            .is_empty());
        assert!(!crate::core::paths::rig_leases_dir()
            .expect("lease dir path")
            .exists());
    });
}
