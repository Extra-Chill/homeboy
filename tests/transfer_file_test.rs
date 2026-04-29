use std::collections::HashMap;

use homeboy::server::transfer::{parse_target, transfer, TransferConfig, TransferTarget};
use homeboy::server::{self, Server};

fn with_home<T>(f: impl FnOnce() -> T) -> T {
    let temp = tempfile::tempdir().expect("temp home");
    let previous = std::env::var("HOME").ok();
    std::env::set_var("HOME", temp.path());
    let result = f();
    match previous {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    result
}

fn save_server(id: &str) {
    server::save(&Server {
        id: id.to_string(),
        aliases: Vec::new(),
        host: "example.test".to_string(),
        user: "deploy".to_string(),
        port: 22,
        identity_file: None,
        env: HashMap::new(),
    })
    .expect("save server");
}

#[test]
fn parses_local_and_remote_transfer_targets() {
    assert_eq!(
        parse_target("prod:/var/www"),
        TransferTarget::Remote {
            server_id: "prod".to_string(),
            path: "/var/www".to_string(),
        }
    );
    assert_eq!(
        parse_target("./artifact.zip"),
        TransferTarget::Local("./artifact.zip".to_string())
    );
    assert_eq!(
        parse_target("relative/artifact.zip"),
        TransferTarget::Local("relative/artifact.zip".to_string())
    );
}

#[test]
fn dry_run_upload_reports_push_without_touching_local_path() {
    with_home(|| {
        save_server("prod");

        let (out, code) = transfer(&TransferConfig {
            source: "./missing-artifact.zip".to_string(),
            destination: "prod:/tmp/artifact.zip".to_string(),
            recursive: false,
            compress: true,
            dry_run: true,
            exclude: Vec::new(),
        })
        .expect("dry run transfer");

        assert_eq!(code, 0);
        assert_eq!(out.direction, "push");
        assert_eq!(out.method, "scp");
        assert!(out.compress);
        assert!(out.dry_run);
        assert!(out.success);
    });
}

#[test]
fn dry_run_remote_to_remote_preserves_recursive_options() {
    with_home(|| {
        save_server("old");
        save_server("new");

        let (out, code) = transfer(&TransferConfig {
            source: "old:/var/www/uploads".to_string(),
            destination: "new:/var/www/uploads".to_string(),
            recursive: true,
            compress: true,
            dry_run: true,
            exclude: vec!["cache".to_string()],
        })
        .expect("dry run server transfer");

        assert_eq!(code, 0);
        assert_eq!(out.direction, "server-to-server");
        assert_eq!(out.method, "tar-pipe");
        assert!(out.recursive);
        assert!(out.compress);
        assert!(out.dry_run);
    });
}
