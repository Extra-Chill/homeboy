use crate::core::component::Component;
use crate::core::release::types::ReleaseOptions;

/// Return true if this component should get a GitHub Release created.
///
/// Resolves the remote URL from the component config (preferred) or from
/// `git remote get-url origin` in the component's local_path, then parses
/// it as a GitHub URL. Non-GitHub remotes (GitLab, self-hosted, etc.) fall
/// through cleanly — the step simply isn't added to the plan.
pub(in crate::core::release) fn github_release_applies(component: &Component) -> bool {
    let remote_url = component.remote_url.clone().or_else(|| {
        crate::core::deploy::release_download::detect_remote_url(std::path::Path::new(
            &component.local_path,
        ))
    });

    remote_url
        .as_deref()
        .and_then(crate::core::deploy::release_download::parse_github_url)
        .is_some()
}

/// Emit plain-language operator hints that separate "package/publish"
/// (registry/package publishing) from "GitHub Release" (the reviewer-facing
/// release page on github.com), so the interaction between `--skip-publish`
/// and `--no-github-release` is unambiguous in the dry-run plan and summary.
///
/// This is messaging only — it never changes which steps run. The actual step
/// inclusion is decided independently by `skip_publish` / `skip_github_release`
/// checks elsewhere in this module (issue #6139).
pub(super) fn push_publish_vs_github_release_hints(
    component: &Component,
    options: &ReleaseOptions,
    publish_targets: &[String],
    hints: &mut Vec<String>,
) {
    let skip_publish = options.pipeline.skip_publish;
    let skip_github_release = options.skip_github_release;
    let github_release_will_run = !skip_github_release && github_release_applies(component);
    let has_publish_targets = !publish_targets.is_empty();

    // Spell out package/publish handling in plain operator language.
    if skip_publish {
        if has_publish_targets {
            hints.push(
                "--skip-publish: skipping registry/package publishing only (no publish to \
                 configured targets). The version bump, tag, and push still run."
                    .to_string(),
            );
        } else {
            hints.push(
                "--skip-publish: registry/package publishing is skipped (no publish targets \
                 configured anyway). The version bump, tag, and push still run."
                    .to_string(),
            );
        }
    }

    // Spell out the GitHub Release outcome separately, and make the
    // skip-publish-without-no-github-release case explicit (the exact footgun
    // from issue #6139: a GitHub Release is STILL created).
    if github_release_will_run {
        if skip_publish {
            hints.push(
                "Note: --skip-publish does NOT skip the GitHub Release — a GitHub Release \
                 (reviewer-facing release page) WILL still be created for this tag. Add \
                 --no-github-release if you want a tag only."
                    .to_string(),
            );
        }
    } else if skip_github_release {
        if skip_publish {
            hints.push(
                "--skip-publish + --no-github-release: outcome is tag-only — no package/registry \
                 publish and no GitHub Release page. The tag is pushed but no reviewer-facing \
                 release will exist."
                    .to_string(),
            );
        } else {
            hints.push(
                "--no-github-release: no GitHub Release (reviewer-facing release page) will be \
                 created. The tag is still pushed."
                    .to_string(),
            );
        }
    }
}
