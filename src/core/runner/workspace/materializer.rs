use crate::core::engine::shell;

use super::util::{owner_capture_shell, owner_restore_shell, parent_remote_path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkspaceMaterializationOperation {
    EnsureParent,
    CleanupOnExit(Vec<String>),
    RecreateTempDir,
    ExtractTarStdinToTemp,
    WriteStdinToBundle,
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
            Self::ExtractTarStdinToTemp => "tar -C \"$tmp\" -xf -".to_string(),
            Self::WriteStdinToBundle => {
                "rm -rf \"$tmp\" \"$bundle\" && cat > \"$bundle\"".to_string()
            }
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
            Self::AtomicReplaceTemp => "rm -rf \"$dest\" && mv \"$tmp\" \"$dest\"".to_string(),
        }
    }
}

fn git_checkout_command(head: &str, branch: Option<&str>) -> String {
    if let Some(branch) = branch {
        format!(
            "git -C \"$tmp\" checkout -B {branch} {head} && git -C \"$tmp\" config branch.{branch}.remote origin && git -C \"$tmp\" config branch.{branch}.merge refs/heads/{branch}",
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
