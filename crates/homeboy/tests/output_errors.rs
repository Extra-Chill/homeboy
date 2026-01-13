use homeboy::output::{map_cmd_result_to_json, CliResponse};
use homeboy::Error;
use homeboy_error::{RemoteCommandFailedDetails, TargetDetails};

#[test]
fn remote_command_failed_serializes_stdout_stderr() {
    let err = Error::remote_command_failed(RemoteCommandFailedDetails {
        command: "ls -la".to_string(),
        exit_code: 127,
        stdout: "some stdout".to_string(),
        stderr: "some stderr".to_string(),
        target: TargetDetails {
            project_id: Some("alpha".to_string()),
            server_id: Some("server1".to_string()),
            host: Some("example.com".to_string()),
        },
    });

    let json = CliResponse::<()>::from_error(&err).to_json();

    assert!(json.contains("\"code\": \"remote.command_failed\""));
    assert!(json.contains("some stdout"));
    assert!(json.contains("some stderr"));
    assert!(json.contains("\"exitCode\": 127"));
}

#[test]
fn remote_command_failed_maps_to_exit_code_20() {
    let err = Error::remote_command_failed(RemoteCommandFailedDetails {
        command: "ls".to_string(),
        exit_code: 1,
        stdout: String::new(),
        stderr: String::new(),
        target: TargetDetails {
            project_id: None,
            server_id: None,
            host: None,
        },
    });

    let (_value, exit_code) = map_cmd_result_to_json::<serde_json::Value>(Err(err));

    assert_eq!(exit_code, 20);
}
