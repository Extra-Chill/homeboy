use homeboy::error::{RemoteCommandFailedDetails, TargetDetails};
use homeboy::Error;

#[test]
fn remote_command_failed_creates_error_with_details() {
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

    assert_eq!(err.code.as_str(), "remote.command_failed");
    assert_eq!(err.message, "Remote command failed");
    // Command details are in the serialized details, not the message
    let details_str = err.details.to_string();
    assert!(details_str.contains("ls -la"));
    assert!(details_str.contains("some stdout"));
    assert!(details_str.contains("some stderr"));
}
