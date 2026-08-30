use std::cell::RefCell;

use super::super::gh_cli::{GhCommandOutput, GitHubReleaseMetadataError, ReleaseAssetPublication};
use super::super::run::{upload_publications_with, PublicationUploadError};

fn publication(name: &str) -> ReleaseAssetPublication {
    ReleaseAssetPublication {
        target_name: name.to_string(),
        sha256: format!("digest-{name}"),
        size: 1,
        source_path: format!("/artifacts/{name}"),
    }
}

fn command(exit_code: i32, stderr: &str) -> GhCommandOutput {
    GhCommandOutput {
        stdout: String::new(),
        stderr: stderr.to_string(),
        exit_code: Some(exit_code),
        timed_out: false,
    }
}

#[test]
fn uploads_are_serial_checkpoints() {
    let events = RefCell::new(Vec::new());
    let publications = [publication("first.zip"), publication("second.zip")];

    let result = upload_publications_with(
        &publications,
        |publication| {
            events
                .borrow_mut()
                .push(format!("upload:{}", publication.target_name));
            command(0, "")
        },
        || {
            events.borrow_mut().push("readback".to_string());
            Ok(())
        },
        |publication, _| {
            events
                .borrow_mut()
                .push(format!("verify:{}", publication.target_name));
            Ok(())
        },
    );

    assert!(result.is_ok());
    assert_eq!(
        events.into_inner(),
        vec![
            "upload:first.zip",
            "readback",
            "verify:first.zip",
            "upload:second.zip",
            "readback",
            "verify:second.zip",
        ]
    );
}

#[test]
fn failed_upload_is_recovered_only_when_readback_verifies_exact_bytes() {
    let publication = publication("large.tar.gz");
    let recovered = upload_publications_with(
        std::slice::from_ref(&publication),
        |_| command(1, "HTTP 404: Not Found"),
        || Ok(()),
        |_, _| Ok(()),
    );
    assert!(recovered.is_ok());

    let rejected = upload_publications_with(
        &[publication],
        |_| command(1, "HTTP 404: Not Found"),
        || Ok(()),
        |_, _| Err(GitHubReleaseMetadataError::message("asset is missing")),
    );
    assert!(matches!(
        rejected,
        Err(PublicationUploadError::Upload {
            readback_error: Some(_),
            ..
        })
    ));
}

#[test]
fn successful_upload_stops_when_readback_cannot_verify_it() {
    let publications = [publication("first.zip"), publication("second.zip")];
    let uploads = RefCell::new(Vec::new());
    let result = upload_publications_with(
        &publications,
        |publication| {
            uploads.borrow_mut().push(publication.target_name.clone());
            command(0, "")
        },
        || Ok(()),
        |_, _| Err(GitHubReleaseMetadataError::message("digest mismatch")),
    );

    assert!(matches!(
        result,
        Err(PublicationUploadError::Verification(_))
    ));
    assert_eq!(uploads.into_inner(), vec!["first.zip"]);
}
