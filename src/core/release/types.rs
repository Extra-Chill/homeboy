mod release_step_type;
mod types;

pub use release_step_type::*;
pub use types::*;

        match s {
            "version" => ReleaseStepType::Version,
            "git.commit" => ReleaseStepType::GitCommit,
            "git.tag" => ReleaseStepType::GitTag,
            "git.push" => ReleaseStepType::GitPush,
            "package" => ReleaseStepType::Package,
            "cleanup" => ReleaseStepType::Cleanup,
            "post_release" => ReleaseStepType::PostRelease,
            other => {
                // Strip "publish." prefix at source - single source of truth for format parsing
                let target = other.strip_prefix("publish.").unwrap_or(other);
                ReleaseStepType::Publish(target.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_str_default_path() {
        let instance = ReleaseStepType::default();
        let _result = instance.from_str();
    }

    #[test]
    fn test_from_str_default_path() {
        let instance = ReleaseStepType::default();
        let _result = instance.from_str();
    }

}
