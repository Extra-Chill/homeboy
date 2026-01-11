use serde::Serialize;

use crate::docs;

use super::CmdResult;

#[derive(Serialize)]
pub struct ChangelogOutput {
    pub topic_label: String,
    pub content: String,
}

pub fn run() -> CmdResult<ChangelogOutput> {
    let (topic_label, content) = docs::resolve(&["changelog".to_string()]);

    if content.is_empty() {
        return Err(homeboy_core::Error::Other(
            "No changelog found (expected embedded docs topic 'changelog')".to_string(),
        ));
    }

    Ok((
        ChangelogOutput {
            topic_label,
            content,
        },
        0,
    ))
}
