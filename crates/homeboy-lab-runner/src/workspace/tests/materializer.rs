use crate::workspace::materializer::{
    dependency_cache_manifest_command, dependency_cache_restore_command,
    dependency_cache_save_command, WorkspaceMaterializationOperation, WorkspaceMaterializer,
};

#[test]
fn workspace_materializer_builds_snapshot_atomic_replace_command() {
    let command = WorkspaceMaterializer::new("/srv/homeboy/_lab_workspaces/homeboy-abc")
        .capture_owner()
        .op(WorkspaceMaterializationOperation::EnsureParent)
        .op(WorkspaceMaterializationOperation::CleanupOnExit(vec![
            "\"$tmp\"".to_string(),
        ]))
        .op(WorkspaceMaterializationOperation::RecreateTempDir)
        .op(WorkspaceMaterializationOperation::ExtractTarStdinToTemp)
        .op(WorkspaceMaterializationOperation::AtomicReplaceTemp)
        .restore_owner()
        .command();

    assert!(command.contains("parent=/srv/homeboy/_lab_workspaces"));
    assert!(command.contains("dest=/srv/homeboy/_lab_workspaces/homeboy-abc"));
    assert!(command.contains("trap 'rm -rf \"$tmp\"' EXIT"));
    assert!(command.contains("tar -C \"$tmp\" -xf -"));
    assert!(command.contains("rm -rf \"$dest\" && mv \"$tmp\" \"$dest\""));
    assert!(command.contains("owner_path=$parent"));
    assert!(command.contains("chown -R \"$owner\" $dest"));
}

#[test]
fn workspace_materializer_builds_git_bundle_checkout_command() {
    let command = WorkspaceMaterializer::new("/srv/homeboy/_lab_workspaces/homeboy-abc")
        .with_bundle_file()
        .capture_owner()
        .op(WorkspaceMaterializationOperation::EnsureParent)
        .op(WorkspaceMaterializationOperation::CleanupOnExit(vec![
            "\"$tmp\"".to_string(),
            "\"$bundle\"".to_string(),
        ]))
        .op(WorkspaceMaterializationOperation::WriteStdinToBundle)
        .op(WorkspaceMaterializationOperation::CloneBundleToTemp)
        .op(WorkspaceMaterializationOperation::SetGitOrigin(
            "https://github.com/Extra-Chill/homeboy.git".to_string(),
        ))
        .op(WorkspaceMaterializationOperation::CheckoutGitRef {
            head: "abc123".to_string(),
            branch: Some("main".to_string()),
        })
        .op(WorkspaceMaterializationOperation::ResetAndCleanGit {
            head: "abc123".to_string(),
        })
        .op(WorkspaceMaterializationOperation::GuardCleanGitWorkspace { allow_dirty: false })
        .op(WorkspaceMaterializationOperation::AtomicReplaceTemp)
        .op(WorkspaceMaterializationOperation::VerifyGitBaseline {
            remote_url: "https://github.com/Extra-Chill/homeboy.git".to_string(),
            head: "abc123".to_string(),
            changed_since_base: None,
        })
        .restore_owner()
        .command();

    assert!(command.contains("bundle=\"${dest}.bundle.$$\""));
    assert!(command.contains("cat > \"$bundle\""));
    assert!(command.contains("git clone \"$bundle\" \"$tmp\""));
    assert!(command.contains("remote set-url origin https://github.com/Extra-Chill/homeboy.git"));
    assert!(command.contains("checkout -B main abc123"));
    assert!(!command.contains("config branch.main.remote origin"));
    assert!(!command.contains("config branch.main.merge refs/heads/main"));
    assert!(command.contains("git -C \"$tmp\" reset --hard abc123"));
    assert!(command.contains("test -d $dest/.git"));
    assert!(command.contains("rev-parse HEAD)\" = abc123"));
    assert!(command.contains("Homeboy Lab refused to overwrite a dirty runner workspace"));
    assert!(command.contains("exit 97"));
}

#[test]
fn workspace_materializer_builds_direct_git_checkout_command() {
    let command = WorkspaceMaterializer::new("/srv/homeboy/_lab_workspaces/homeboy-abc")
        .capture_owner()
        .op(WorkspaceMaterializationOperation::EnsureParent)
        .op(WorkspaceMaterializationOperation::SyncGitCheckout {
            remote_url: "https://github.com/Extra-Chill/homeboy.git".to_string(),
            head: "abc123".to_string(),
            branch: None,
            changed_since_base: Some("origin/main".to_string()),
            fetch_refs: vec!["refs/pull/123/head:refs/remotes/pull/123".to_string()],
            allow_dirty: false,
        })
        .op(WorkspaceMaterializationOperation::VerifyGitBaseline {
            remote_url: "https://github.com/Extra-Chill/homeboy.git".to_string(),
            head: "abc123".to_string(),
            changed_since_base: Some("origin/main".to_string()),
        })
        .restore_owner()
        .command();

    assert!(command.contains("parent=/srv/homeboy/_lab_workspaces"));
    assert!(command.contains("dest=/srv/homeboy/_lab_workspaces/homeboy-abc"));
    assert!(command.contains("mkdir -p \"$parent\""));
    assert!(command.contains("if [ -d \"$dest\"/.git ]; then"));
    assert!(command.contains("Homeboy Lab refused to overwrite a dirty runner workspace"));
    assert!(command.contains("git clone https://github.com/Extra-Chill/homeboy.git \"$dest\""));
    assert!(command.contains("fetch --prune origin '+refs/heads/*:refs/remotes/origin/*'"));
    assert!(command.contains("fetch origin refs/pull/123/head:refs/remotes/pull/123"));
    assert!(command.contains("rev-parse --verify -q 'origin/main^{commit}'"));
    assert!(command.contains("checkout --detach abc123"));
    assert!(command.contains("git -C \"$dest\" clean -ffdqx"));
    assert!(command.contains("test -d $dest/.git"));
    assert!(command.contains("config --get remote.origin.url"));
    assert!(command.contains("chown -R \"$owner\" $dest"));
}

#[test]
fn workspace_materializer_builds_dependency_cache_restore_command() {
    let command = dependency_cache_restore_command(
        "/srv/homeboy/_lab_workspaces/homeboy-abc",
        "vendor/cache with spaces",
        "/srv/homeboy/_dependency_cache/key/vendor__cache.tar",
    );

    assert!(command.contains("dest=/srv/homeboy/_lab_workspaces/homeboy-abc"));
    assert!(command.contains("mkdir -p \"$dest\""));
    assert!(command.contains("rm -rf \"$dest\"/'vendor/cache with spaces'"));
    assert!(command
        .contains("tar -C \"$dest\" -xf /srv/homeboy/_dependency_cache/key/vendor__cache.tar"));
}

#[test]
fn workspace_materializer_builds_dependency_cache_save_command() {
    let command = dependency_cache_save_command(
        "/srv/homeboy/_lab_workspaces/homeboy-abc",
        "/srv/homeboy/_dependency_cache/key",
        "vendor/cache with spaces",
        "/srv/homeboy/_dependency_cache/key/vendor__cache.tar",
    );

    assert!(command.contains("dest=/srv/homeboy/_dependency_cache/key"));
    assert!(command.contains("mkdir -p \"$dest\""));
    assert!(command.contains("mkdir -p /srv/homeboy/_dependency_cache/key"));
    assert!(command.contains(
        "tar -C /srv/homeboy/_lab_workspaces/homeboy-abc -cf /srv/homeboy/_dependency_cache/key/vendor__cache.tar 'vendor/cache with spaces'"
    ));
}

#[test]
fn workspace_materializer_builds_dependency_cache_manifest_command() {
    let command = dependency_cache_manifest_command(
        "/srv/homeboy/_dependency_cache/key",
        "{\n  \"key\": \"dep-cache\"\n}",
    );

    assert!(command.contains("dest=/srv/homeboy/_dependency_cache/key"));
    assert!(command.contains("mkdir -p \"$dest\""));
    assert!(command.contains("printf %s '{"));
    assert!(command.contains("}' > \"$dest\"/manifest.json"));
}
