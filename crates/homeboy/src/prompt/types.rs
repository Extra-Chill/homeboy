/// A yes/no confirmation prompt.
pub struct YesNoPrompt {
    pub question: String,
    /// true = default yes [Y/n], false = default no [y/N]
    pub default: bool,
}

/// Select one option from a list.
pub struct SelectPrompt {
    pub question: String,
    pub options: Vec<SelectOption>,
    pub default_index: Option<usize>,
}

pub struct SelectOption {
    pub value: String,
    pub label: String,
}

/// Review a list of items and confirm.
pub struct ConfirmListPrompt {
    pub header: String,
    pub items: Vec<String>,
    pub confirm_question: String,
    pub default: bool,
}

/// Free text input.
pub struct TextPrompt {
    pub question: String,
    pub default: Option<String>,
}

/// A complete interactive flow (series of prompts).
pub struct PromptFlow {
    pub steps: Vec<PromptStep>,
}

pub enum PromptStep {
    YesNo(YesNoPrompt),
    Select(SelectPrompt),
    ConfirmList(ConfirmListPrompt),
    Text(TextPrompt),
    Message(String),
}
