use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn write_rig(package: &Path, id: &str, body: &str) -> PathBuf {
    let rig_dir = package.join("rigs").join(id);
    fs::create_dir_all(&rig_dir).expect("rig dir");
    let rig_path = rig_dir.join("rig.json");
    fs::write(&rig_path, body).expect("rig json");
    rig_path
}

pub(crate) fn minimal_rig(id: &str) -> String {
    format!(
        r#"{{
            "id": "{}",
            "description": "{} rig",
            "components": {{
                "app": {{ "path": "${{env.DEV_ROOT}}/{}" }}
            }},
            "pipeline": {{
                "check": [{{ "kind": "check", "label": "app exists", "file": "${{components.app.path}}" }}]
            }}
        }}"#,
        id, id, id
    )
}

pub(crate) fn minimal_stack(id: &str, component: &str) -> String {
    format!(
        r#"{{
            "id": "{}",
            "description": "{} stack",
            "component": "{}",
            "component_path": "${{env.DEV_ROOT}}/{}",
            "base": {{ "remote": "origin", "branch": "main" }},
            "target": {{ "remote": "origin", "branch": "dev/combined-fixes" }},
            "prs": []
        }}"#,
        id, id, component, component
    )
}

pub(crate) fn write_stack(package: &Path, id: &str, component: &str) -> PathBuf {
    let stacks_dir = package.join("stacks");
    fs::create_dir_all(&stacks_dir).expect("stacks dir");
    let stack_path = stacks_dir.join(format!("{}.json", id));
    fs::write(&stack_path, minimal_stack(id, component)).expect("stack json");
    stack_path
}

pub(crate) fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
