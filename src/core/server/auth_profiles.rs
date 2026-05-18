//! Reusable keychain-backed auth profiles for generic HTTP requests.

use crate::core::error::{Error, ErrorCode, Result};
use crate::core::keychain;
use base64::Engine;
use serde::Serialize;

const PROFILE_KIND: &str = "kind";
const PROFILE_USERNAME: &str = "username";
const PROFILE_PASSWORD: &str = "password";
const PROFILE_TOKEN: &str = "token";

#[derive(Debug, Serialize)]
pub struct ProfileSetResult {
    pub profile: String,
    pub kind: String,
    pub stored: bool,
}

#[derive(Debug, Serialize)]
pub struct ProfileStatusResult {
    pub profile: String,
    pub kind: Option<String>,
    pub available: bool,
}

#[derive(Debug, Serialize)]
pub struct ProfileRemoveResult {
    pub profile: String,
    pub removed: usize,
}

fn profile_scope(profile: &str) -> String {
    format!("profile:{}", profile)
}

pub fn set_profile_basic(
    profile: &str,
    username: &str,
    password: &str,
) -> Result<ProfileSetResult> {
    let scope = profile_scope(profile);
    keychain::set(&scope, PROFILE_KIND, "basic")?;
    keychain::set(&scope, PROFILE_USERNAME, username)?;
    keychain::set(&scope, PROFILE_PASSWORD, password)?;
    Ok(ProfileSetResult {
        profile: profile.to_string(),
        kind: "basic".to_string(),
        stored: true,
    })
}

pub fn set_profile_bearer(profile: &str, token: &str) -> Result<ProfileSetResult> {
    let scope = profile_scope(profile);
    keychain::set(&scope, PROFILE_KIND, "bearer")?;
    keychain::set(&scope, PROFILE_TOKEN, token)?;
    Ok(ProfileSetResult {
        profile: profile.to_string(),
        kind: "bearer".to_string(),
        stored: true,
    })
}

pub fn profile_status(profile: &str) -> Result<ProfileStatusResult> {
    let scope = profile_scope(profile);
    let kind = keychain::get(&scope, PROFILE_KIND)?;
    let available = match kind.as_deref() {
        Some("basic") => {
            keychain::get(&scope, PROFILE_USERNAME)?.is_some()
                && keychain::get(&scope, PROFILE_PASSWORD)?.is_some()
        }
        Some("bearer") => keychain::get(&scope, PROFILE_TOKEN)?.is_some(),
        _ => false,
    };

    Ok(ProfileStatusResult {
        profile: profile.to_string(),
        kind,
        available,
    })
}

pub fn remove_profile(profile: &str) -> Result<ProfileRemoveResult> {
    let scope = profile_scope(profile);
    let variables = vec![
        PROFILE_KIND.to_string(),
        PROFILE_USERNAME.to_string(),
        PROFILE_PASSWORD.to_string(),
        PROFILE_TOKEN.to_string(),
    ];
    let removed = keychain::remove_many(&scope, &variables)?;
    Ok(ProfileRemoveResult {
        profile: profile.to_string(),
        removed,
    })
}

pub fn profile_authorization_header(profile: &str) -> Result<String> {
    let scope = profile_scope(profile);
    let kind =
        keychain::get(&scope, PROFILE_KIND)?.ok_or_else(|| missing_profile_error(profile))?;
    match kind.as_str() {
        "basic" => {
            let username = keychain::get(&scope, PROFILE_USERNAME)?
                .ok_or_else(|| missing_profile_error(profile))?;
            let password = keychain::get(&scope, PROFILE_PASSWORD)?
                .ok_or_else(|| missing_profile_error(profile))?;
            let encoded = base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", username, password));
            Ok(format!("Basic {}", encoded))
        }
        "bearer" => {
            let token = keychain::get(&scope, PROFILE_TOKEN)?
                .ok_or_else(|| missing_profile_error(profile))?;
            Ok(format!("Bearer {}", token))
        }
        other => Err(Error::validation_invalid_argument(
            "auth-profile",
            format!("Unsupported auth profile kind '{}'", other),
            Some(profile.to_string()),
            Some(vec!["basic".to_string(), "bearer".to_string()]),
        )),
    }
}

fn missing_profile_error(profile: &str) -> Error {
    Error::new(
        ErrorCode::ExtensionNotFound,
        format!("Auth profile '{}' is not set", profile),
        serde_json::Value::Null,
    )
    .with_hint(format!(
        "Run 'homeboy auth profile set-basic {} --username <user>' or 'homeboy auth profile set-bearer {}'",
        profile, profile
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_scope() {
        assert_eq!(profile_scope("matticspace"), "profile:matticspace");
    }

    #[test]
    #[ignore]
    fn test_set_profile_basic() {
        let profile = "homeboy-auth-profile-basic-test";
        let _ = remove_profile(profile);

        let result = set_profile_basic(profile, "user", "secret").expect("store basic profile");

        assert!(result.stored);
        assert_eq!(result.kind, "basic");
        remove_profile(profile).expect("cleanup profile");
    }

    #[test]
    #[ignore]
    fn test_set_profile_bearer() {
        let profile = "homeboy-auth-profile-bearer-test";
        let _ = remove_profile(profile);

        let result = set_profile_bearer(profile, "token").expect("store bearer profile");

        assert!(result.stored);
        assert_eq!(result.kind, "bearer");
        remove_profile(profile).expect("cleanup profile");
    }

    #[test]
    #[ignore]
    fn test_profile_status() {
        let profile = "homeboy-auth-profile-status-test";
        let _ = remove_profile(profile);
        set_profile_bearer(profile, "token").expect("store bearer profile");

        let result = profile_status(profile).expect("profile status");

        assert!(result.available);
        assert_eq!(result.kind.as_deref(), Some("bearer"));
        remove_profile(profile).expect("cleanup profile");
    }

    #[test]
    #[ignore]
    fn test_profile_authorization_header() {
        let profile = "homeboy-auth-profile-header-test";
        let _ = remove_profile(profile);
        set_profile_basic(profile, "user", "secret").expect("store basic profile");

        let header = profile_authorization_header(profile).expect("authorization header");

        assert_eq!(header, "Basic dXNlcjpzZWNyZXQ=");
        remove_profile(profile).expect("cleanup profile");
    }

    #[test]
    #[ignore]
    fn test_remove_profile() {
        let profile = "homeboy-auth-profile-remove-test";
        set_profile_bearer(profile, "token").expect("store bearer profile");

        let result = remove_profile(profile).expect("remove profile");

        assert!(result.removed > 0);
    }
}
