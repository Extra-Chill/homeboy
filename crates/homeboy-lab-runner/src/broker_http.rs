use reqwest::blocking::RequestBuilder;

use homeboy_core::broker_auth::BROKER_TOKEN_HEADER;

/// Attach the paired broker bearer token, when present, to an outgoing broker
/// request. Sent via both the canonical header and `Authorization: Bearer` so
/// the request works through proxies that strip one or the other.
pub(crate) fn with_broker_token(builder: RequestBuilder, token: Option<&str>) -> RequestBuilder {
    match token {
        Some(token) if !token.trim().is_empty() => builder
            .header(BROKER_TOKEN_HEADER, token)
            .bearer_auth(token),
        _ => builder,
    }
}
