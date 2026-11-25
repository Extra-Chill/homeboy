use crate::workspace::materializer::{WorkspaceMaterializationOperation, WorkspaceMaterializer};

#[test]
fn workspace_materializer_fetches_only_the_exact_sha_for_a_fresh_checkout() {
    let command = WorkspaceMaterializer::new("/srv/homeboy/_lab_workspaces/homeboy-abc")
        .op(WorkspaceMaterializationOperation::SyncGitCheckout {
            remote_url: "https://github.com/Extra-Chill/homeboy.git".to_string(),
            head: "abc123".to_string(),
            branch: None,
            changed_since_base: None,
            fetch_refs: Vec::new(),
            allow_dirty: false,
        })
        .command();

    // A fresh checkout is built in the staging path `$tmp` and only atomically
    // renamed into `$dest` once HEAD is valid, so a cancelled clone never leaves
    // a partial `$dest` (#8886). The reuse branch (existing `.git`) still
    // operates in place.
    assert!(command.contains("git init \"$tmp\""));
    assert!(command.contains("remote add origin https://github.com/Extra-Chill/homeboy.git"));
    assert!(command.contains("fetch --filter=blob:none origin abc123"));
    assert!(command.contains("checkout --detach abc123"));
    // The fresh clone validates HEAD, then atomically publishes tmp -> dest.
    assert!(command.contains("git -C \"$tmp\" rev-parse --verify -q HEAD"));
    assert!(command.contains("mv \"$tmp\" \"$dest\""));
    // The reuse branch still resets an existing valid checkout in place.
    assert!(command.contains("git -C \"$dest\" reset --hard"));
    assert!(!command.contains("git clone"));
}
