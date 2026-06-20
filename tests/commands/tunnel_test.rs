use super::service::{
    run_service, ServiceTunnelAuthModeArg, ServiceTunnelPreviewPolicyArg, TunnelServiceCommand,
};
use super::*;
use crate::test_support;
use homeboy::core::server::Server;
use std::collections::HashMap;
use std::fs;

fn create_server() {
    homeboy::core::server::save(&Server {
        id: "private-host".to_string(),
        aliases: Vec::new(),
        host: "private.example.test".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        kind: None,
        auth: None,
        env: HashMap::new(),
        runner: None,
    })
    .expect("save server");
}

#[test]
fn expose_service_command_records_declaration() {
    test_support::with_isolated_home(|_| {
        create_server();
        let (output, exit_code) = run_service(TunnelServiceCommand::Expose {
            id: "site-preview".to_string(),
            server: Some("private-host".to_string()),
            runner_local: false,
            remote_host: "127.0.0.1".to_string(),
            remote_port: 7331,
            scheme: "http".to_string(),
            local_port: Some(8831),
            auth_mode: ServiceTunnelAuthModeArg::BearerEnv,
            auth_env: Some("SITE_PREVIEW_TOKEN".to_string()),
            auth_header: Some("Authorization".to_string()),
            allowed_clients: vec!["app-runtime".to_string()],
            description: None,
            preview_policy: ServiceTunnelPreviewPolicyArg::None,
            preview_keep_alive_until: None,
        })
        .expect("command succeeds");

        assert_eq!(exit_code, 0);
        assert_eq!(output.command, "tunnel.service.expose");
        assert_eq!(output.entity.expect("entity").id, "site-preview");
    });
}

#[test]
fn expose_service_command_supports_runner_local_without_server() {
    test_support::with_isolated_home(|_| {
        let (output, exit_code) = run_service(TunnelServiceCommand::Expose {
            id: "runner-local-preview".to_string(),
            server: None,
            runner_local: true,
            remote_host: "127.0.0.1".to_string(),
            remote_port: 7331,
            scheme: "http".to_string(),
            local_port: Some(8831),
            auth_mode: ServiceTunnelAuthModeArg::SshOnly,
            auth_env: None,
            auth_header: None,
            allowed_clients: Vec::new(),
            description: None,
            preview_policy: ServiceTunnelPreviewPolicyArg::None,
            preview_keep_alive_until: None,
        })
        .expect("runner-local expose succeeds without server declaration");

        assert_eq!(exit_code, 0);
        assert_eq!(output.command, "tunnel.service.expose");
        assert_eq!(output.entity.expect("entity").id, "runner-local-preview");
    });
}

#[test]
fn preview_consumer_run_uses_configured_public_url_and_artifacts() {
    test_support::with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("checkout");
        let script = checkout.path().join("consumer.mjs");
        fs::write(
            &script,
            r#"
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
const artifacts = process.argv[process.argv.indexOf('--artifacts') + 1];
const publicUrl = process.argv[process.argv.indexOf('--public-url') + 1];
mkdirSync(artifacts, { recursive: true });
writeFileSync(join(artifacts, 'result.json'), JSON.stringify({ public_result_url: `${publicUrl}/result` }));
console.log(`Public result URL: ${publicUrl}/result`);
"#,
        )
        .expect("script");
        let artifacts = tempfile::tempdir().expect("artifacts");
        let config = tempfile::NamedTempFile::new().expect("config");
        fs::write(
            config.path(),
            serde_json::json!({
                "id": "sample-consumer",
                "command": {
                    "program": "node",
                    "args": [
                        script.display().to_string(),
                        "--public-url",
                        "${preview_public_url}",
                        "--artifacts",
                        "${artifacts_dir}"
                    ],
                    "artifacts_dir": artifacts.path()
                },
                "output": {
                    "public_result_json_file": "result.json",
                    "public_result_json_pointer": "/public_result_url",
                    "public_result_stdout_prefix": "Public result URL:"
                }
            })
            .to_string(),
        )
        .expect("config json");

        let (output, exit_code) = run_preview_consumer_config(
            config.path().to_path_buf(),
            None,
            Some("https://run.example.test".to_string()),
            None,
            false,
            None,
        )
        .expect("preview consumer command succeeds");

        assert_eq!(exit_code, 0);
        let result = output
            .extra
            .preview_consumer
            .expect("preview consumer output");
        assert_eq!(
            result.public_result_url.as_deref(),
            Some("https://run.example.test/result")
        );
        assert_eq!(result.exit_code, Some(0));
        assert!(matches!(
            result.status,
            preview_consumer::PreviewConsumerStatus::Completed
        ));
        assert!(result.preview_ready);
        assert!(artifacts
            .path()
            .join("homeboy-preview-consumer.json")
            .exists());
    });
}

#[test]
fn preview_consumer_non_blocking_reports_running_without_waiting_for_exit() {
    test_support::with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("checkout");
        let script = checkout.path().join("held-consumer.mjs");
        fs::write(
            &script,
            r#"
const publicUrl = process.argv[process.argv.indexOf('--public-url') + 1];
console.log(`Public result URL: ${publicUrl}/result`);
// Stay alive to emulate a held preview runtime.
setTimeout(() => {}, 60_000);
"#,
        )
        .expect("script");
        let artifacts = tempfile::tempdir().expect("artifacts");
        let config = tempfile::NamedTempFile::new().expect("config");
        fs::write(
            config.path(),
            serde_json::json!({
                "id": "held-consumer",
                "command": {
                    "program": "node",
                    "args": [
                        script.display().to_string(),
                        "--public-url",
                        "${preview_public_url}"
                    ],
                    "artifacts_dir": artifacts.path()
                },
                "output": {
                    "public_result_stdout_prefix": "Public result URL:"
                }
            })
            .to_string(),
        )
        .expect("config json");

        let (output, exit_code) = run_preview_consumer_config(
            config.path().to_path_buf(),
            None,
            Some("https://run.example.test".to_string()),
            None,
            true,
            Some(30),
        )
        .expect("preview consumer command succeeds");

        assert_eq!(exit_code, 0);
        let result = output
            .extra
            .preview_consumer
            .expect("preview consumer output");
        assert!(matches!(
            result.status,
            preview_consumer::PreviewConsumerStatus::Running
        ));
        assert!(result.preview_ready);
        assert_eq!(
            result.public_result_url.as_deref(),
            Some("https://run.example.test/result")
        );
        assert!(result.exit_code.is_none());
        assert!(result.pid.is_some());

        if let Some(pid) = result.pid {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status();
        }
    });
}
