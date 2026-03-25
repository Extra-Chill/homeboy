//! trait_impls — extracted from mod.rs.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use crate::core::component::Component;
use crate::core::component::RawComponent;


impl From<RawComponent> for Component {
    fn from(raw: RawComponent) -> Self {
        let mut hooks = raw.hooks;
        merge_legacy_hook(
            &mut hooks,
            "pre:version:bump",
            raw.pre_version_bump_commands,
        );
        merge_legacy_hook(
            &mut hooks,
            "post:version:bump",
            raw.post_version_bump_commands,
        );
        merge_legacy_hook(&mut hooks, "post:release", raw.post_release_commands);

        Component {
            id: raw.id,
            aliases: raw.aliases,
            local_path: raw.local_path,
            remote_path: raw.remote_path,
            build_artifact: raw.build_artifact,
            extensions: raw.extensions,
            version_targets: raw.version_targets,
            changelog_target: raw.changelog_target,
            changelog_next_section_label: raw.changelog_next_section_label,
            changelog_next_section_aliases: raw.changelog_next_section_aliases,
            hooks,
            extract_command: raw.extract_command,
            remote_owner: raw.remote_owner,
            deploy_strategy: raw.deploy_strategy,
            git_deploy: raw.git_deploy,
            remote_url: raw.remote_url,
            auto_cleanup: raw.auto_cleanup,
            docs_dir: raw.docs_dir,
            docs_dirs: raw.docs_dirs,
            scopes: raw.scopes,
        }
    }
}
