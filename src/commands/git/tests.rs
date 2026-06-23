use super::args::{PrArgs, PrCommand};
use super::GitCommand;
use clap::Parser;

#[derive(Parser)]
struct TestCli {
    #[command(subcommand)]
    command: GitCommand,
}

#[test]
fn push_override_flags_parse() {
    let cli = TestCli::try_parse_from([
        "git",
        "push",
        "homeboy",
        "--remote-url",
        "https://github.com/Extra-Chill/homeboy",
        "--token",
        "secret-token",
        "--refspec",
        "HEAD:refs/heads/autofix",
        "--strip-extraheader",
    ])
    .expect("push flags parse");

    match cli.command {
        GitCommand::Push {
            component_id,
            remote_url,
            token,
            refspec,
            strip_extraheader,
            ..
        } => {
            assert_eq!(component_id.as_deref(), Some("homeboy"));
            assert_eq!(
                remote_url.as_deref(),
                Some("https://github.com/Extra-Chill/homeboy")
            );
            assert_eq!(token.as_deref(), Some("secret-token"));
            assert_eq!(refspec.as_deref(), Some("HEAD:refs/heads/autofix"));
            assert!(strip_extraheader);
        }
        _ => panic!("expected push command"),
    }
}

#[test]
fn push_token_requires_remote_url_at_parse_time() {
    let err = match TestCli::try_parse_from(["git", "push", "homeboy", "--token", "secret-token"]) {
        Ok(_) => panic!("--token should require --remote-url"),
        Err(err) => err,
    };

    assert!(err.to_string().contains("--remote-url"));
}

#[test]
fn pr_readiness_flags_parse() {
    let cli = TestCli::try_parse_from([
        "git",
        "pr",
        "readiness",
        "homeboy",
        "--number",
        "5805",
        "--path",
        "/tmp/homeboy",
    ])
    .expect("pr readiness flags parse");

    match cli.command {
        GitCommand::Pr(PrArgs {
            command:
                PrCommand::Readiness {
                    component_id,
                    number,
                    path,
                },
        }) => {
            assert_eq!(component_id, "homeboy");
            assert_eq!(number, 5805);
            assert_eq!(path.as_deref(), Some("/tmp/homeboy"));
        }
        _ => panic!("expected pr readiness command"),
    }
}
