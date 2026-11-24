use homeboy_core::defaults;
use homeboy_core::engine::shell;
use homeboy_core::error::posix_quote_arg;
use homeboy_core::error::Error;

use super::install_script::{shown, Step, StepOutput, LOG_MARKER};

/// Install-script steps that normalize ownership, then modes, of a deployed tree.
pub(crate) fn fix_deployed_permissions_steps(
    remote_path: &str,
    remote_owner: Option<&str>,
) -> Vec<Step> {
    let root = resolved_root(remote_path);
    let perms = defaults::load_defaults().permissions.remote;
    vec![
        Step::best_effort(ownership_command(remote_path, remote_owner)),
        chmod_step(
            format!(
                "{root}; find \"$root\" -type d -exec chmod {} {{}} + 2>/dev/null",
                perms.dir_mode
            ),
            "chmod directories",
            remote_path,
        ),
        chmod_step(
            format!(
                "{root}; find \"$root\" -type f -exec chmod {} {{}} + 2>/dev/null",
                perms.file_mode
            ),
            "chmod files",
            remote_path,
        ),
    ]
}

/// Shell that sets `$root` to the tree to normalize. A symlinked target (the
/// versioned release layout, #18365) resolves through its `/.` entry so
/// `find` and `chown -R` walk the release it points at instead of stopping at
/// the link; a real directory is used unchanged.
fn resolved_root(remote_path: &str) -> String {
    format!(
        "root={}; if [ -L \"$root\" ]; then root=\"$root/.\"; fi",
        shell::quote_path(remote_path)
    )
}

fn chmod_step(command: String, operation: &'static str, remote_path: &str) -> Step {
    let remote_path = remote_path.to_string();
    Step::new(command, move |output: &StepOutput| {
        Err(Error::remote_command_failed(
            homeboy_core::error::RemoteCommandFailedDetails {
                command: format!("{} on {}", operation, remote_path),
                exit_code: output.exit_code,
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
                target: homeboy_core::error::TargetDetails {
                    project_id: None,
                    server_id: None,
                    host: None,
                },
            },
        ))
    })
}

/// Best-effort ownership fix. Uses the configured owner, otherwise inherits
/// the PARENT directory's owner: after a deploy the target is owned by the
/// deploy user, while the parent (e.g. `wp-content/plugins/`) keeps the web
/// server's ownership. An unprivileged user that cannot chown still applies
/// the group so multi-writer access survives.
fn ownership_command(remote_path: &str, remote_owner: Option<&str>) -> String {
    let log = |message: &str| format!("echo \"{LOG_MARKER}{message}\"");
    let resolve_owner = match remote_owner {
        Some(owner) => format!("owner={}", posix_quote_arg(owner)),
        None => {
            let parent_path = remote_path
                .trim_end_matches('/')
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .unwrap_or(remote_path);
            let parent = shell::quote_path(parent_path);
            let shown = shown(parent_path);
            format!(
                "owner=$(stat -c '%U:%G' {parent} 2>/dev/null || stat -f '%Su:%Sg' {parent} 2>/dev/null)\n\
                 if [ -z \"$owner\" ]; then {skip}; exit 0; fi\n\
                 if [ \"$owner\" = root:root ]; then {root}; exit 0; fi\n\
                 {detected}",
                skip = log(&format!(
                    "Could not detect ownership of parent {shown}, skipping chown"
                )),
                root = log(&format!(
                    "Parent directory {shown} is root:root — set remote_owner on the component to fix ownership"
                )),
                detected = log(&format!("Auto-detected ownership $owner from parent {shown}")),
            )
        }
    };
    let shown_target = shown(remote_path);
    let root = resolved_root(remote_path);
    format!(
        "{resolve_owner}\n\
         {setting}\n\
         {root}\n\
         chown -R \"$owner\" \"$root\" 2>/dev/null; rc=$?\n\
         if [ \"$rc\" -ne 0 ]; then\n\
         {chown_failed}\n\
         case \"$owner\" in *:*)\n\
         chgrp -R \"${{owner##*:}}\" \"$root\" 2>/dev/null; rc=$?\n\
         if [ \"$rc\" -ne 0 ]; then {chgrp_failed}; fi ;;\n\
         esac\n\
         fi",
        setting = log(&format!("Setting ownership to $owner on {shown_target}")),
        chown_failed = log("Warning: chown failed (exit $rc): "),
        chgrp_failed = log("Warning: chgrp failed (exit $rc): "),
    )
}
