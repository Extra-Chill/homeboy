use super::{run, FileArgs, FileCommand, FileCommandOutput};
use crate::commands::GlobalArgs;
use crate::test_support::with_isolated_home;

#[test]
fn file_read_json_includes_size_metadata() {
    let project_root = tempfile::tempdir().expect("project tempdir");
    let project_id = "local-file-read";
    let content = "hello\nworld";

    std::fs::write(project_root.path().join("sample.txt"), content).expect("write sample file");

    let result = with_isolated_home(|home| {
        let project_dir = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("projects")
            .join(project_id);
        std::fs::create_dir_all(&project_dir).expect("create project config dir");
        let config = serde_json::json!({
            "base_path": project_root.path().to_string_lossy(),
        });
        std::fs::write(
            project_dir.join(format!("{project_id}.json")),
            serde_json::to_vec(&config).expect("serialize project config"),
        )
        .expect("write project config");

        run(
            FileArgs {
                command: FileCommand::Read {
                    project_id: project_id.to_string(),
                    path: "sample.txt".to_string(),
                    _json: true,
                    raw: false,
                },
            },
            &GlobalArgs {},
        )
    });

    let (output, code) = result.expect("run homeboy file read");
    let FileCommandOutput::Standard(payload) = output else {
        panic!("expected standard file output");
    };
    let expected_path = project_root
        .path()
        .join("sample.txt")
        .to_string_lossy()
        .to_string();

    assert_eq!(code, 0);
    assert_eq!(payload.command, "file.read");
    assert_eq!(payload.path.as_deref(), Some(expected_path.as_str()));
    assert_eq!(payload.content.as_deref(), Some(content));
    assert_eq!(payload.size, Some(content.len() as i64));
}
