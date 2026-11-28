use std::path::Path;

use homeboy_core::error::{Error, Result};

#[derive(Default)]
pub(super) struct BuildIgnore(Vec<Rule>);

struct Rule {
    pattern: String,
    anchored: bool,
    exclude: bool,
}

impl BuildIgnore {
    pub(super) fn load(component_path: &Path) -> Result<Self> {
        let path = component_path.join(".buildignore");
        match std::fs::read_to_string(&path) {
            Ok(contents) => Ok(Self::parse(&contents)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(Error::internal_io(
                format!("Failed to read .buildignore for package completeness: {error}"),
                Some(path.display().to_string()),
            )),
        }
    }

    fn parse(contents: &str) -> Self {
        let mut rules = Vec::new();
        for line in contents.lines() {
            if line.is_empty() || line.starts_with(['#', ';']) {
                continue;
            }
            // The WordPress packager strips the trailing slash before handing
            // the rules to rsync --exclude-from. Keep the leading root anchor.
            let line = line.strip_suffix('/').unwrap_or(line);
            if line == "!" {
                rules.clear();
                continue;
            }
            let (exclude, pattern) = match line.strip_prefix("+ ") {
                Some(pattern) => (false, pattern),
                None => (true, line.strip_prefix("- ").unwrap_or(line)),
            };
            let anchored = pattern.starts_with('/');
            let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
            if !pattern.is_empty() {
                rules.push(Rule {
                    pattern: pattern.to_string(),
                    anchored,
                    exclude,
                });
            }
        }
        Self(rules)
    }

    pub(super) fn excludes(&self, path: &str) -> bool {
        if self.0.is_empty() {
            return false;
        }
        // Check ancestor directories too: an include for a file cannot bring
        // it back if the packager never descends into its excluded parent.
        path.match_indices('/')
            .map(|(end, _)| (&path[..end], true))
            .chain(std::iter::once((path, false)))
            .any(|(candidate, directory)| {
                self.0
                    .iter()
                    .find(|rule| rule.matches(candidate, directory))
                    .is_some_and(|rule| rule.exclude)
            })
    }
}

impl Rule {
    fn matches(&self, path: &str, directory: bool) -> bool {
        if let Some(parent) = self.pattern.strip_suffix("/***") {
            // rsync's trailing /*** matches both the directory and its contents.
            return (directory && self.matches_pattern(parent, path))
                || path
                    .match_indices('/')
                    .any(|(end, _)| self.matches_pattern(parent, &path[..end]));
        }
        self.matches_pattern(&self.pattern, path)
    }

    fn matches_pattern(&self, pattern: &str, path: &str) -> bool {
        if !self.anchored && !pattern.contains('/') && !pattern.contains("**") {
            return matches_glob(pattern, path.rsplit('/').next().unwrap_or(path));
        }
        matches_glob(pattern, path)
            || (!self.anchored
                && path
                    .match_indices('/')
                    .any(|(index, _)| matches_glob(pattern, &path[index + 1..])))
    }
}

fn matches_glob(pattern: &str, path: &str) -> bool {
    if !pattern.contains(['*', '?', '[']) {
        return pattern == path;
    }
    // rsync does not use glob-match's brace expansion or leading negation.
    let pattern = pattern.replace('{', "\\{");
    if pattern.starts_with('!') {
        glob_match::glob_match(&format!("\\{pattern}"), path)
    } else {
        glob_match::glob_match(&pattern, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buildignore_matches_path_boundaries_and_root_anchors() {
        let ignore = BuildIgnore::parse("# comment\r\n\r\n/src/\r\nassets/src/\r\n*.map.css\r\n");
        for path in [
            "src/ContactForm.css",
            "assets/src/contact.css",
            "nested/assets/src/contact.css",
            "assets/contact.map.css",
        ] {
            assert!(ignore.excludes(path), "{path}");
        }
        for path in [
            "nested/src/contact.css",
            "src-extra/contact.css",
            "assets/src.css",
            "assets/contact-form.css",
        ] {
            assert!(!ignore.excludes(path), "{path}");
        }
    }

    #[test]
    fn buildignore_uses_first_matching_rule_for_each_path_component() {
        let ignore = BuildIgnore::parse(
            "*.php\n!\n; comment\n+ /assets/keep.css\n- /assets/*.css\n+ src/keep.css\nsrc/***\n",
        );
        assert!(!ignore.excludes("plugin.php"));
        assert!(!ignore.excludes("assets/keep.css"));
        assert!(ignore.excludes("assets/omit.css"));
        assert!(ignore.excludes("src/keep.css"));
    }
}
