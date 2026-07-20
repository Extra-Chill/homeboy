use homeboy_core::engine::shell;

use super::util::{owner_capture_shell, owner_restore_shell, parent_remote_path};

const CACHE_MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkspaceMaterializationOperation {
    EnsureParent,
    CleanupOnExit(Vec<String>),
    RecreateTempDir,
    ExtractTarStdinToTemp,
    WriteStdinToBundle,
    VerifyBundleDigest(String),
    CloneBundleToTemp,
    SetGitOrigin(String),
    CheckoutGitRef {
        head: String,
        branch: Option<String>,
    },
    ResetAndCleanGit {
        head: String,
    },
    GuardCleanGitWorkspace {
        allow_dirty: bool,
    },
    VerifyGitBaseline {
        remote_url: String,
        head: String,
        changed_since_base: Option<String>,
    },
    SyncGitCheckout {
        remote_url: String,
        head: String,
        branch: Option<String>,
        changed_since_base: Option<String>,
        fetch_refs: Vec<String>,
        allow_dirty: bool,
    },
    EnsureDestDir,
    EnsureParentOf(String),
    RestoreDependencyCachePath {
        path: String,
        archive: String,
    },
    SaveDependencyCachePath {
        remote_path: String,
        path: String,
        archive: String,
    },
    WriteDependencyCacheManifest {
        json: String,
    },
    AtomicReplaceTemp,
}

#[derive(Debug, Clone)]
pub(super) struct WorkspaceMaterializer {
    remote_path: String,
    bundle: bool,
    capture_owner: bool,
    restore_owner: bool,
    operations: Vec<WorkspaceMaterializationOperation>,
}

impl WorkspaceMaterializer {
    pub(super) fn new(remote_path: impl Into<String>) -> Self {
        Self {
            remote_path: remote_path.into(),
            bundle: false,
            capture_owner: false,
            restore_owner: false,
            operations: Vec::new(),
        }
    }

    pub(super) fn with_bundle_file(mut self) -> Self {
        self.bundle = true;
        self
    }

    pub(super) fn capture_owner(mut self) -> Self {
        self.capture_owner = true;
        self
    }

    pub(super) fn restore_owner(mut self) -> Self {
        self.restore_owner = true;
        self
    }

    pub(super) fn op(mut self, operation: WorkspaceMaterializationOperation) -> Self {
        self.operations.push(operation);
        self
    }

    pub(super) fn command(&self) -> String {
        let parent = parent_remote_path(&self.remote_path);
        let mut prefix = format!(
            "parent={parent}; dest={dest}; tmp=\"${{dest}}.tmp.$$\"",
            parent = shell::quote_arg(parent.as_str()),
            dest = shell::quote_arg(&self.remote_path),
        );
        if self.bundle {
            prefix.push_str("; bundle=\"${dest}.bundle.$$\"");
            // Bundles are controller-complete transfers. A runner must not
            // lazily recover a missing object from the source remote.
            prefix.push_str("; export GIT_NO_LAZY_FETCH=1");
        }
        if self.capture_owner {
            prefix.push_str("; ");
            prefix.push_str(&owner_capture_shell("$parent"));
        }

        let mut commands = Vec::new();
        for operation in &self.operations {
            commands.push(operation.shell());
        }
        if self.restore_owner {
            commands.push(owner_restore_shell("$parent", "$dest"));
        }

        format!("{prefix}; {}", commands.join(" && "))
    }
}

impl WorkspaceMaterializationOperation {
    fn shell(&self) -> String {
        match self {
            Self::EnsureParent => "mkdir -p \"$parent\"".to_string(),
            Self::CleanupOnExit(paths) => format!(
                "trap 'rm -rf {}' EXIT",
                paths
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            Self::RecreateTempDir => "rm -rf \"$tmp\" && mkdir -p \"$tmp\"".to_string(),
            // v3 snapshot provenance binds the owner execute capability. Tar's
            // default extraction applies the runner umask, which can erase that
            // bit before the verifier sees the materialized tree.
            Self::ExtractTarStdinToTemp => "tar -p -C \"$tmp\" -xf -".to_string(),
            Self::WriteStdinToBundle => {
                "rm -rf \"$tmp\" \"$bundle\" && cat > \"$bundle\"".to_string()
            }
            Self::VerifyBundleDigest(expected) => format!(
                "test \"$(shasum -a 256 \"$bundle\" | awk '{{print $1}}')\" = {expected}",
                expected = shell::quote_arg(expected)
            ),
            Self::CloneBundleToTemp => "git clone \"$bundle\" \"$tmp\"".to_string(),
            Self::SetGitOrigin(remote_url) => format!(
                "git -C \"$tmp\" remote set-url origin {remote_url}",
                remote_url = shell::quote_arg(remote_url)
            ),
            Self::CheckoutGitRef { head, branch } => git_checkout_command(head, branch.as_deref()),
            Self::ResetAndCleanGit { head } => format!(
                "git -C \"$tmp\" reset --hard {head} && git -C \"$tmp\" clean -ffdqx",
                head = shell::quote_arg(head)
            ),
            Self::GuardCleanGitWorkspace { allow_dirty } => {
                dirty_git_workspace_guard("$dest", *allow_dirty)
            }
            Self::VerifyGitBaseline {
                remote_url,
                head,
                changed_since_base,
            } => verify_git_baseline_command(
                "$dest",
                remote_url,
                head,
                changed_since_base.as_deref(),
            ),
            Self::SyncGitCheckout {
                remote_url,
                head,
                branch,
                changed_since_base,
                fetch_refs,
                allow_dirty,
            } => sync_git_checkout_command(
                remote_url,
                head,
                branch.as_deref(),
                changed_since_base.as_deref(),
                fetch_refs,
                *allow_dirty,
            ),
            Self::EnsureDestDir => "mkdir -p \"$dest\"".to_string(),
            Self::EnsureParentOf(path) => format!(
                "mkdir -p {parent}",
                parent = shell::quote_arg(&parent_remote_path(path))
            ),
            Self::RestoreDependencyCachePath { path, archive } => format!(
                "rm -rf {target} && tar -C \"$dest\" -xf {archive}",
                target = dest_child_path(path),
                archive = shell::quote_arg(archive)
            ),
            Self::SaveDependencyCachePath {
                remote_path,
                path,
                archive,
            } => format!(
                "tar -C {remote} -cf {archive} {path}",
                remote = shell::quote_arg(remote_path),
                archive = shell::quote_arg(archive),
                path = shell::quote_arg(path),
            ),
            Self::WriteDependencyCacheManifest { json } => format!(
                "printf %s {json} > {manifest}",
                json = shell::quote_arg(json),
                manifest = dest_child_path(CACHE_MANIFEST_FILE),
            ),
            Self::AtomicReplaceTemp => "rm -rf \"$dest\" && mv \"$tmp\" \"$dest\"".to_string(),
        }
    }
}

fn verify_git_baseline_command(
    dest: &str,
    remote_url: &str,
    head: &str,
    changed_since_base: Option<&str>,
) -> String {
    let status = format!(
        "git -C {dest} status --porcelain=v1 2>/dev/null | while IFS= read -r line; do path=${{line#???}}; if [ \"$path\" = .homeboy ] || [ \"${{path#.homeboy/}}\" != \"$path\" ]; then :; else printf '%s\\n' \"$line\"; fi; done || true",
        dest = dest,
    );
    let changed_since = changed_since_base.map_or_else(String::new, |base| {
        format!(
            " && git -C {dest} rev-parse --verify -q {base}^{{commit}} >/dev/null && test \"$(git -C {dest} merge-base {base} HEAD)\" = {base}",
            base = shell::quote_arg(base),
        )
    });
    format!(
        "test -d {dest}/.git && test \"$(git -C {dest} rev-parse HEAD)\" = {head} && test \"$(git -C {dest} config --get remote.origin.url)\" = {remote_url}{changed_since} && test -z \"$({status})\"",
        dest = dest,
        head = shell::quote_arg(head),
        remote_url = shell::quote_arg(remote_url),
        changed_since = changed_since,
        status = status,
    )
}

pub(crate) fn dependency_cache_restore_command(
    remote_path: &str,
    path: &str,
    archive: &str,
) -> String {
    WorkspaceMaterializer::new(remote_path)
        .op(WorkspaceMaterializationOperation::EnsureDestDir)
        .op(
            WorkspaceMaterializationOperation::RestoreDependencyCachePath {
                path: path.to_string(),
                archive: archive.to_string(),
            },
        )
        .command()
}

pub(crate) fn dependency_cache_save_command(
    remote_path: &str,
    cache_path: &str,
    path: &str,
    archive: &str,
) -> String {
    WorkspaceMaterializer::new(cache_path)
        .op(WorkspaceMaterializationOperation::EnsureDestDir)
        .op(WorkspaceMaterializationOperation::EnsureParentOf(
            archive.to_string(),
        ))
        .op(WorkspaceMaterializationOperation::SaveDependencyCachePath {
            remote_path: remote_path.to_string(),
            path: path.to_string(),
            archive: archive.to_string(),
        })
        .command()
}

pub(crate) fn dependency_cache_manifest_command(cache_path: &str, json: &str) -> String {
    WorkspaceMaterializer::new(cache_path)
        .op(WorkspaceMaterializationOperation::EnsureDestDir)
        .op(
            WorkspaceMaterializationOperation::WriteDependencyCacheManifest {
                json: json.to_string(),
            },
        )
        .command()
}

fn dest_child_path(path: &str) -> String {
    format!("\"$dest\"/{}", shell::quote_arg(path))
}

fn git_checkout_command(head: &str, branch: Option<&str>) -> String {
    if let Some(branch) = branch {
        format!(
            "git -C \"$tmp\" checkout -B {branch} {head}",
            branch = shell::quote_arg(branch),
            head = shell::quote_arg(head),
        )
    } else {
        format!(
            "git -C \"$tmp\" checkout --detach {head}",
            head = shell::quote_arg(head)
        )
    }
}

pub(super) fn dirty_git_workspace_guard(dest: &str, allow_dirty: bool) -> String {
    let status = format!(
        "git -C {dest} status --porcelain=v1 2>/dev/null | while IFS= read -r line; do path=${{line#???}}; if [ \"$path\" = .homeboy ] || [ \"${{path#.homeboy/}}\" != \"$path\" ]; then :; else printf '%s\\n' \"$line\"; fi; done || true",
        dest = dest,
    );
    if allow_dirty {
        format!(
            "dirty=$({status}); if [ -n \"$dirty\" ]; then printf '%s\\n' 'Homeboy Lab warning: --allow-dirty-lab-workspace is overwriting uncommitted runner workspace changes.' >&2; printf '%s\\n' \"$dirty\" >&2; fi",
            status = status,
        )
    } else {
        format!(
            "dirty=$({status}); if [ -n \"$dirty\" ]; then printf '%s\\n' 'Homeboy Lab refused to overwrite a dirty runner workspace.' >&2; printf '%s\\n' \"$dirty\" >&2; printf '%s\\n' 'Commit, stash, clean, or remove the runner workspace before retrying. Pass --allow-dirty-lab-workspace only for noisy investigation that may discard runner-side changes.' >&2; exit 97; fi",
            status = status,
        )
    }
}

fn sync_git_checkout_command(
    remote_url: &str,
    head: &str,
    branch: Option<&str>,
    changed_since_base: Option<&str>,
    fetch_refs: &[String],
    allow_dirty: bool,
) -> String {
    if changed_since_base.is_none() && fetch_refs.is_empty() {
        return fresh_exact_git_checkout_command(remote_url, head, branch, allow_dirty);
    }

    let fetch_changed_since = changed_since_base
        .map(|base| {
            format!(
                " && (git -C \"$dest\" rev-parse --verify -q {} >/dev/null || git -C \"$dest\" fetch origin {})",
                shell::quote_arg(&format!("{base}^{{commit}}")),
                shell::quote_arg(base)
            )
        })
        .unwrap_or_default();
    let fetch_extra_refs = fetch_refs
        .iter()
        .map(|git_ref| {
            format!(
                " && git -C \"$dest\" fetch origin {}",
                shell::quote_arg(git_ref)
            )
        })
        .collect::<String>();
    let dirty_guard = dirty_git_workspace_guard("$dest", allow_dirty);

    let checkout = branch.map_or_else(
        || {
            format!(
                "git -C \"$dest\" checkout --detach {head}",
                head = shell::quote_arg(head)
            )
        },
        |branch| {
            format!(
                "git -C \"$dest\" checkout -B {branch} {head}",
                branch = shell::quote_arg(branch),
                head = shell::quote_arg(head),
            )
        },
    );
    format!(
        "if [ -d \"$dest\"/.git ]; then {dirty_guard} && git -C \"$dest\" reset --hard && git -C \"$dest\" clean -ffdqx && git -C \"$dest\" fetch --prune origin '+refs/heads/*:refs/remotes/origin/*'; else rm -rf \"$dest\" && git clone {remote_url} \"$dest\" && git -C \"$dest\" fetch --prune origin '+refs/heads/*:refs/remotes/origin/*'; fi{fetch_extra_refs}{fetch_changed_since} && {checkout} && git -C \"$dest\" reset --hard {head} && git -C \"$dest\" clean -ffdqx",
        remote_url = shell::quote_arg(remote_url),
        head = shell::quote_arg(head),
        checkout = checkout,
    )
}

fn fresh_exact_git_checkout_command(
    remote_url: &str,
    head: &str,
    branch: Option<&str>,
    allow_dirty: bool,
) -> String {
    // Re-use branch: an existing `.git` at `$dest` is already a valid checkout,
    // so resetting/cleaning it in place is safe and cheap.
    let reuse_checkout = branch.map_or_else(
        || {
            format!(
                "git -C \"$dest\" checkout --detach {head}",
                head = shell::quote_arg(head)
            )
        },
        |branch| {
            format!(
                "git -C \"$dest\" checkout -B {branch} {head}",
                branch = shell::quote_arg(branch),
                head = shell::quote_arg(head),
            )
        },
    );
    // Fresh-clone branch: build the whole checkout in the staging path `$tmp`
    // and only atomically rename it into `$dest` once HEAD is valid. A cancelled
    // or timed-out fresh clone leaves at most a partial `$tmp`, never a partial
    // `$dest` that `workspace list` would advertise as reusable (#8886).
    let tmp_checkout = branch.map_or_else(
        || {
            format!(
                "git -C \"$tmp\" checkout --detach {head}",
                head = shell::quote_arg(head)
            )
        },
        |branch| {
            format!(
                "git -C \"$tmp\" checkout -B {branch} {head}",
                branch = shell::quote_arg(branch),
                head = shell::quote_arg(head),
            )
        },
    );
    let reuse = format!(
        "{dirty_guard} && git -C \"$dest\" reset --hard && git -C \"$dest\" clean -ffdqx && git -C \"$dest\" fetch --prune origin '+refs/heads/*:refs/remotes/origin/*' && {reuse_checkout} && git -C \"$dest\" reset --hard {head} && git -C \"$dest\" clean -ffdqx",
        dirty_guard = dirty_git_workspace_guard("$dest", allow_dirty),
        reuse_checkout = reuse_checkout,
        head = shell::quote_arg(head),
    );
    let fresh = format!(
        "rm -rf \"$tmp\" && git init \"$tmp\" && git -C \"$tmp\" remote add origin {remote_url} && git -C \"$tmp\" fetch --filter=blob:none origin {head} && {tmp_checkout} && git -C \"$tmp\" reset --hard {head} && git -C \"$tmp\" clean -ffdqx && git -C \"$tmp\" rev-parse --verify -q HEAD >/dev/null && rm -rf \"$dest\" && mv \"$tmp\" \"$dest\"",
        remote_url = shell::quote_arg(remote_url),
        head = shell::quote_arg(head),
        tmp_checkout = tmp_checkout,
    );
    format!("if [ -d \"$dest\"/.git ]; then {reuse}; else {fresh}; fi")
}
