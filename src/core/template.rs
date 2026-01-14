use std::collections::HashMap;

pub struct TemplateVars;

impl TemplateVars {
    pub const PROJECT_ID: &'static str = "projectId";
    pub const ARGS: &'static str = "args";
    pub const DOMAIN: &'static str = "domain";
    pub const SITE_PATH: &'static str = "sitePath";
    pub const CLI_PATH: &'static str = "cliPath";
    pub const TABLE: &'static str = "table";
    pub const QUERY: &'static str = "query";
    pub const FORMAT: &'static str = "format";
    pub const TARGET_DIR: &'static str = "targetDir";
}

pub fn render(template: &str, variables: &[(&str, &str)]) -> String {
    let mut result = template.to_string();

    for (key, value) in variables {
        let placeholder = format!("{{{{{}}}}}", key);
        result = result.replace(&placeholder, value);
    }

    result
}

pub fn render_map(template: &str, variables: &HashMap<String, String>) -> String {
    let mut result = template.to_string();

    for (key, value) in variables {
        let placeholder = format!("{{{{{}}}}}", key);
        result = result.replace(&placeholder, value);
    }

    result
}

pub fn is_present(template: &str, key: &str) -> bool {
    let placeholder = format!("{{{{{}}}}}", key);
    template.contains(&placeholder)
}
