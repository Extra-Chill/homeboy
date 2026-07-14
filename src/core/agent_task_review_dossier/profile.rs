use crate::core::error::Result;

use super::model::{AgentTaskReviewProfile, AgentTaskReviewSectionId};
use super::validation::validate_profile;

pub fn default_profile() -> AgentTaskReviewProfile {
    AgentTaskReviewProfile {
        required_sections: vec![
            AgentTaskReviewSectionId::Summary,
            AgentTaskReviewSectionId::WhatChanged,
            AgentTaskReviewSectionId::HowToTest,
            AgentTaskReviewSectionId::Compatibility,
            AgentTaskReviewSectionId::AiAssistance,
        ],
        ..Default::default()
    }
}

/// The component's portable config is the only profile source. Invalid portable
/// config therefore fails finalization instead of being mistaken for profile absence.
pub fn resolve_review_profile(path: &str) -> Result<AgentTaskReviewProfile> {
    let component = crate::core::component::resolve_effective(None, Some(path), None)?;
    let profile = component.review_profile.unwrap_or_else(default_profile);
    validate_profile(&profile)?;
    Ok(profile)
}
