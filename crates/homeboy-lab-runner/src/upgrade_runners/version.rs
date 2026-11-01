use regex::Regex;

pub fn parse_cli_version_output(output: &str) -> Option<String> {
    let re = Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
    re.find(output).map(|m| m.as_str().to_string())
}
