use std::collections::HashMap;

pub struct TemplateVars;

impl TemplateVars {
    pub const PROJECT_ID: &'static str = "projectId";
    pub const ARGS: &'static str = "args";
    pub const DOMAIN: &'static str = "domain";
    pub const TARGET_DOMAIN: &'static str = "targetDomain";
    pub const SITE_PATH: &'static str = "sitePath";
    pub const CLI_PATH: &'static str = "cliPath";
    pub const DB_USER: &'static str = "dbUser";
    pub const DB_PASSWORD: &'static str = "dbPassword";
    pub const DB_NAME: &'static str = "dbName";
    pub const DB_HOST: &'static str = "dbHost";
    pub const TABLE: &'static str = "table";
    pub const QUERY: &'static str = "query";
    pub const FORMAT: &'static str = "format";
    pub const TARGET_DIR: &'static str = "targetDir";
    pub const SCRIPT: &'static str = "script";
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
