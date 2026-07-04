//! Trust model for the reverse runner broker (`/runner/*` routes).
//!
//! The reverse runner broker lets a Homeboy controller submit jobs and remote
//! Lab workers claim/execute/finish them over HTTP. On loopback or private
//! tunnels that is acceptable, but a VPS-hosted broker is reachable beyond the
//! trust boundary and therefore must authenticate every caller (#2990).
//!
//! ## Trust model
//!
//! * **Pairing.** An operator pairs a worker by minting a *runner credential*
//!   on the broker host (`broker_auth_pair`). Pairing generates a random bearer
//!   token, stores its SHA-256 hash for enforcement, and binds it to a single
//!   `runner_id` with a set of [`BrokerScope`]s. The controller copy may retain
//!   the plaintext token so controller-side submit/file requests can attach it;
//!   enforcement-host installs strip plaintext token material and keep hashes
//!   only.
//! * **Controller submit.** Job submission (`POST /runner/jobs`) is authorized
//!   by a credential carrying the [`BrokerScope::Submit`] scope. Worker claims
//!   are authorized by [`BrokerScope::Work`]. A credential may hold both.
//! * **Runner-id binding.** Worker routes carry a `runner_id` in their body.
//!   The presented token must belong to a credential whose `runner_id` matches,
//!   so a paired runner can never claim, progress, or finish jobs on behalf of a
//!   different runner id.
//! * **Revocation.** Credentials are revocable by id; revoked credentials are
//!   retained (so their ids stay reserved) but reject every request.
//! * **Secure by default.** If no auth store exists the broker refuses all
//!   `/runner/*` traffic. An operator must explicitly opt a credential-less
//!   broker into loopback-only smoke mode, which is gated to loopback binds.
//!
//! Tokens live in `~/.config/homeboy/broker_auth.json` with `0600` perms on
//! Unix. Plaintext token material is only kept in the controller-side copy and
//! is never installed on runner enforcement hosts.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::error::{Error, Result};
use crate::core::paths;

/// Header carrying the broker bearer token. `Authorization: Bearer <token>` is
/// also accepted; this header is the canonical, proxy-friendly form.
pub const BROKER_TOKEN_HEADER: &str = "x-homeboy-broker-token";

/// Environment variable a controller reads its broker submit token from. Kept
/// in the environment (not config) so the secret never lands in serialized,
/// printed config.
pub const BROKER_TOKEN_ENV: &str = "HOMEBOY_BROKER_TOKEN";

/// Resolve the controller-side broker token from the environment, if set.
pub fn broker_token_from_env() -> Option<String> {
    std::env::var(BROKER_TOKEN_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Resolve the controller-side broker submit token for `runner_id`.
///
/// `HOMEBOY_BROKER_TOKEN` remains an explicit override. Otherwise, use the
/// newest active submit credential for the runner whose controller-local token
/// still matches its persisted hash.
pub fn broker_submit_token_for_runner(runner_id: &str) -> Result<Option<String>> {
    if let Some(token) = broker_token_from_env() {
        return Ok(Some(token));
    }
    let store = BrokerAuthStore::load()?;
    Ok(store.submit_token_for_runner(runner_id))
}

/// Authorization scopes a broker credential may grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerScope {
    /// Controller-side job submission (`POST /runner/jobs`).
    Submit,
    /// Worker-side register/claim/event/finish/heartbeat/cancel.
    Work,
}

impl BrokerScope {
    fn as_str(self) -> &'static str {
        match self {
            BrokerScope::Submit => "submit",
            BrokerScope::Work => "work",
        }
    }
}

/// A single paired credential. Only the token *hash* is persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerCredential {
    /// Stable credential id (operator-facing label for revocation).
    pub id: String,
    /// Runner id this credential is bound to. Worker routes must match it.
    pub runner_id: String,
    /// Lowercase hex SHA-256 of the bearer token. Never the plaintext.
    pub token_sha256: String,
    /// Controller-local plaintext token for attaching authenticated submit
    /// requests. Stripped before installing the store on broker enforcement
    /// hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Granted scopes.
    #[serde(default)]
    pub scopes: BTreeSet<BrokerScope>,
    /// When set, the credential is disabled and rejects every request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
    /// Creation timestamp (RFC3339).
    pub created_at: String,
}

impl BrokerCredential {
    fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// On-disk broker auth store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrokerAuthStore {
    #[serde(default)]
    pub credentials: Vec<BrokerCredential>,
    /// Explicit opt-in to run without any credentials. Only honored for
    /// loopback binds; keeps existing local/tunnel smoke setups working without
    /// silently disabling auth on an exposed broker.
    #[serde(default)]
    pub allow_unauthenticated_loopback: bool,
}

/// Result of a successful authorization: the matched credential and the scope
/// the request was checked against.
#[derive(Debug, Clone)]
pub struct BrokerAuthGrant {
    pub credential_id: String,
    pub runner_id: String,
}

impl BrokerAuthStore {
    /// Load the store, returning an empty (unconfigured) store if absent.
    pub fn load() -> Result<Self> {
        let path = store_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("read {}", path.display())))
        })?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw)
            .map_err(|err| Error::config_invalid_json(path.display().to_string(), err))
    }

    /// Persist the store with restrictive permissions.
    pub fn save(&self) -> Result<PathBuf> {
        let path = store_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!("create {}", parent.display())),
                )
            })?;
        }
        let serialized = serde_json::to_string_pretty(self).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("serialize broker auth store".to_string()),
            )
        })?;
        std::fs::write(&path, serialized).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("write {}", path.display())))
        })?;
        restrict_permissions(&path)?;
        Ok(path)
    }

    fn configured(&self) -> bool {
        self.credentials.iter().any(BrokerCredential::is_active)
    }

    /// Authorize a request for `required` scope, optionally bound to
    /// `runner_id`. `presented` is the bearer token extracted from the request
    /// headers (already trimmed of any `Bearer ` prefix), if any.
    pub fn authorize(
        &self,
        loopback_bind: bool,
        presented: Option<&str>,
        required: BrokerScope,
        runner_id: Option<&str>,
    ) -> Result<BrokerAuthGrant> {
        if !self.configured() {
            if self.allow_unauthenticated_loopback && loopback_bind {
                return Ok(BrokerAuthGrant {
                    credential_id: "loopback-open".to_string(),
                    runner_id: runner_id.unwrap_or_default().to_string(),
                });
            }
            return Err(Error::broker_auth_denied(
                "broker has no paired runner credentials configured",
                runner_id.map(str::to_string),
                vec![
                    "Pair and install a runner credential with `homeboy runner broker pair <id> --runner-id <runner> --submit --work`, or use `--no-install` only when you will copy broker_auth.json to the broker enforcement host yourself."
                        .to_string(),
                    "For loopback-only smoke runs, set allow_unauthenticated_loopback in broker_auth.json."
                        .to_string(),
                ],
            ));
        }

        let Some(token) = presented.map(str::trim).filter(|t| !t.is_empty()) else {
            return Err(Error::broker_auth_denied(
                "missing broker bearer token",
                runner_id.map(str::to_string),
                vec![format!(
                    "Send the paired token via `{BROKER_TOKEN_HEADER}` or `Authorization: Bearer <token>`."
                )],
            ));
        };

        let presented_hash = sha256_hex(token);
        let Some(credential) = self
            .credentials
            .iter()
            .filter(|cred| cred.is_active())
            .find(|cred| constant_time_eq(&cred.token_sha256, &presented_hash))
        else {
            return Err(Error::broker_auth_denied(
                "broker token is not recognized or has been revoked",
                runner_id.map(str::to_string),
                vec!["Re-pair the runner to mint a fresh token.".to_string()],
            ));
        };

        if !credential.scopes.contains(&required) {
            return Err(Error::broker_auth_denied(
                format!(
                    "broker credential lacks required `{}` scope",
                    required.as_str()
                ),
                Some(credential.runner_id.clone()),
                vec![format!(
                    "Re-pair `{}` with the `{}` scope.",
                    credential.runner_id,
                    required.as_str()
                )],
            ));
        }

        if let Some(requested_runner) = runner_id {
            if credential.runner_id != requested_runner {
                return Err(Error::broker_auth_denied(
                    "broker token is bound to a different runner id",
                    Some(requested_runner.to_string()),
                    vec![
                        "A paired runner may only act on its own runner id; claims cannot be stolen."
                            .to_string(),
                    ],
                ));
            }
        }

        Ok(BrokerAuthGrant {
            credential_id: credential.id.clone(),
            runner_id: credential.runner_id.clone(),
        })
    }

    /// Mint and store a new credential, returning the one-time plaintext token.
    pub fn pair(
        &mut self,
        id: impl Into<String>,
        runner_id: impl Into<String>,
        scopes: BTreeSet<BrokerScope>,
    ) -> Result<MintedCredential> {
        let id = id.into();
        let runner_id = runner_id.into();
        if id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "id",
                "broker credential id must not be empty",
                None,
                None,
            ));
        }
        if runner_id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "runner_id",
                "broker credential requires a runner id",
                None,
                None,
            ));
        }
        if scopes.is_empty() {
            return Err(Error::validation_invalid_argument(
                "scopes",
                "broker credential requires at least one scope",
                None,
                Some(vec!["Grant `submit`, `work`, or both.".to_string()]),
            ));
        }
        if self
            .credentials
            .iter()
            .any(|cred| cred.id == id && cred.is_active())
        {
            return Err(Error::validation_invalid_argument(
                "id",
                format!("an active broker credential `{id}` already exists"),
                Some(id.clone()),
                Some(vec!["Revoke it first or choose a different id.".to_string()]),
            ));
        }

        let token = generate_token();
        let credential = BrokerCredential {
            id: id.clone(),
            runner_id: runner_id.clone(),
            token_sha256: sha256_hex(&token),
            token: Some(token.clone()),
            scopes,
            revoked_at: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.credentials.push(credential);
        Ok(MintedCredential {
            id,
            runner_id,
            token,
        })
    }

    /// Revoke an active credential by id. Returns true when a credential was
    /// transitioned to revoked.
    pub fn revoke(&mut self, id: &str) -> bool {
        let now = chrono::Utc::now().to_rfc3339();
        let mut revoked = false;
        for cred in self.credentials.iter_mut() {
            if cred.id == id && cred.is_active() {
                cred.revoked_at = Some(now.clone());
                revoked = true;
            }
        }
        revoked
    }

    pub fn enforcement_copy(&self) -> Self {
        let mut copy = self.clone();
        for credential in &mut copy.credentials {
            credential.token = None;
        }
        copy
    }

    pub fn submit_token_for_runner(&self, runner_id: &str) -> Option<String> {
        self.credentials
            .iter()
            .rev()
            .filter(|credential| {
                credential.is_active()
                    && credential.runner_id == runner_id
                    && credential.scopes.contains(&BrokerScope::Submit)
            })
            .find_map(|credential| {
                let token = credential.token.as_deref()?.trim();
                if token.is_empty() || sha256_hex(token) != credential.token_sha256 {
                    return None;
                }
                Some(token.to_string())
            })
    }
}

/// The one-time result of minting a credential. The plaintext `token` must be
/// delivered to the worker immediately; it cannot be recovered later.
#[derive(Debug, Clone)]
pub struct MintedCredential {
    pub id: String,
    pub runner_id: String,
    pub token: String,
}

/// Extract a bearer token from request header lines, supporting both the
/// canonical `x-homeboy-broker-token` header and `Authorization: Bearer ...`.
pub fn extract_bearer_token(header_lines: &str) -> Option<String> {
    for line in header_lines.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == BROKER_TOKEN_HEADER {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        } else if name == "authorization" {
            if let Some(token) = value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
            {
                let token = token.trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}

/// Path to the broker auth store on disk.
pub fn store_path() -> Result<PathBuf> {
    Ok(paths::homeboy()?.join("broker_auth.json"))
}

fn sha256_hex(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    format!("{digest:x}")
}

/// Length-independent constant-time string compare for hashes to avoid leaking
/// match progress via timing.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Generate a 256-bit-equivalent random token from two UUIDv4 values. Avoids a
/// new RNG dependency while staying high-entropy and url-safe.
fn generate_token() -> String {
    format!(
        "hbk_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("set permissions {}", path.display())),
        )
    })
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scopes(values: &[BrokerScope]) -> BTreeSet<BrokerScope> {
        values.iter().copied().collect()
    }

    fn store_with(runner_id: &str, scope: BrokerScope) -> (BrokerAuthStore, String) {
        let mut store = BrokerAuthStore::default();
        let minted = store
            .pair("cred-1", runner_id, scopes(&[scope]))
            .expect("pair");
        (store, minted.token)
    }

    #[test]
    fn unconfigured_store_rejects_by_default() {
        let store = BrokerAuthStore::default();
        let err = store
            .authorize(true, None, BrokerScope::Work, Some("runner-a"))
            .expect_err("secure by default");
        assert_eq!(err.code.as_str(), "broker.auth_denied");
        assert!(err.hints.iter().any(|hint| {
            hint.message.contains("--no-install") && hint.message.contains("broker_auth.json")
        }));
    }

    #[test]
    fn loopback_open_opt_in_allows_unauthenticated_loopback_only() {
        let store = BrokerAuthStore {
            allow_unauthenticated_loopback: true,
            ..Default::default()
        };
        // Loopback bind: permitted.
        store
            .authorize(true, None, BrokerScope::Work, Some("runner-a"))
            .expect("loopback open");
        // Non-loopback bind: still rejected even with opt-in.
        let err = store
            .authorize(false, None, BrokerScope::Work, Some("runner-a"))
            .expect_err("non-loopback rejected");
        assert_eq!(err.code.as_str(), "broker.auth_denied");
    }

    #[test]
    fn missing_token_is_rejected_when_configured() {
        let (store, _token) = store_with("runner-a", BrokerScope::Work);
        let err = store
            .authorize(false, None, BrokerScope::Work, Some("runner-a"))
            .expect_err("missing token");
        assert!(err.message.contains("missing broker bearer token"));
    }

    #[test]
    fn paired_runner_can_authorize_its_own_runner_id() {
        let (store, token) = store_with("runner-a", BrokerScope::Work);
        let grant = store
            .authorize(false, Some(&token), BrokerScope::Work, Some("runner-a"))
            .expect("authorized");
        assert_eq!(grant.runner_id, "runner-a");
    }

    #[test]
    fn wrong_runner_id_cannot_claim() {
        let (store, token) = store_with("runner-a", BrokerScope::Work);
        let err = store
            .authorize(false, Some(&token), BrokerScope::Work, Some("runner-b"))
            .expect_err("cross-runner claim rejected");
        assert!(err.message.contains("different runner id"));
    }

    #[test]
    fn wrong_token_is_rejected() {
        let (store, _token) = store_with("runner-a", BrokerScope::Work);
        let err = store
            .authorize(
                false,
                Some("hbk_bogus"),
                BrokerScope::Work,
                Some("runner-a"),
            )
            .expect_err("bad token");
        assert!(err.message.contains("not recognized"));
    }

    #[test]
    fn scope_is_enforced() {
        let (store, token) = store_with("runner-a", BrokerScope::Work);
        let err = store
            .authorize(false, Some(&token), BrokerScope::Submit, None)
            .expect_err("submit needs submit scope");
        assert!(err.message.contains("scope"));
    }

    #[test]
    fn revoked_credential_is_rejected() {
        let (mut store, token) = store_with("runner-a", BrokerScope::Work);
        assert!(store.revoke("cred-1"));
        let err = store
            .authorize(false, Some(&token), BrokerScope::Work, Some("runner-a"))
            .expect_err("revoked");
        assert!(
            err.message.contains("no paired runner credentials") || err.message.contains("revoked")
        );
    }

    #[test]
    fn submit_scope_authorizes_controller_submit_without_runner_binding() {
        let (store, token) = store_with("runner-a", BrokerScope::Submit);
        store
            .authorize(false, Some(&token), BrokerScope::Submit, None)
            .expect("submit authorized");
    }

    #[test]
    fn extract_bearer_supports_both_header_forms() {
        let token = extract_bearer_token("Authorization: Bearer abc123\r\nHost: x").expect("auth");
        assert_eq!(token, "abc123");
        let token =
            extract_bearer_token("X-Homeboy-Broker-Token: tok-xyz\r\nHost: x").expect("custom");
        assert_eq!(token, "tok-xyz");
        assert!(extract_bearer_token("Host: x").is_none());
    }

    #[test]
    fn minted_token_is_high_entropy_and_controller_store_keeps_plaintext() {
        let (store, token) = store_with("runner-a", BrokerScope::Work);
        assert!(token.starts_with("hbk_"));
        assert!(token.len() > 32);
        assert_eq!(store.credentials[0].token_sha256, sha256_hex(&token));
        assert_eq!(store.credentials[0].token.as_deref(), Some(token.as_str()));
    }

    #[test]
    fn enforcement_copy_strips_plaintext_tokens() {
        let (store, token) = store_with("runner-a", BrokerScope::Work);
        let enforcement_store = store.enforcement_copy();

        assert_eq!(store.credentials[0].token.as_deref(), Some(token.as_str()));
        assert!(enforcement_store
            .credentials
            .iter()
            .all(|credential| credential.token.is_none()));
        assert_eq!(
            enforcement_store.credentials[0].token_sha256,
            sha256_hex(&token)
        );
    }

    #[test]
    fn submit_token_for_runner_selects_newest_matching_submit_credential() {
        let mut store = BrokerAuthStore::default();
        store
            .pair("cred-1", "runner-a", scopes(&[BrokerScope::Submit]))
            .expect("pair first");
        let newest = store
            .pair(
                "cred-2",
                "runner-a",
                scopes(&[BrokerScope::Submit, BrokerScope::Work]),
            )
            .expect("pair second");
        store
            .pair("cred-3", "runner-b", scopes(&[BrokerScope::Submit]))
            .expect("pair other runner");

        assert_eq!(
            store.submit_token_for_runner("runner-a").as_deref(),
            Some(newest.token.as_str())
        );
    }

    #[test]
    fn submit_token_for_runner_ignores_missing_or_mismatched_plaintext() {
        let mut store = BrokerAuthStore::default();
        let usable = store
            .pair("cred-1", "runner-a", scopes(&[BrokerScope::Submit]))
            .expect("pair first");
        store
            .pair("cred-2", "runner-a", scopes(&[BrokerScope::Submit]))
            .expect("pair second");
        store.credentials[1].token = Some("wrong-token".to_string());

        assert_eq!(
            store.submit_token_for_runner("runner-a").as_deref(),
            Some(usable.token.as_str())
        );
    }

    #[test]
    fn saved_store_round_trips_hashed_credential_for_enforcement() {
        crate::test_support::with_isolated_home(|_| {
            let mut store = BrokerAuthStore::default();
            let minted = store
                .pair(
                    "cred-1",
                    "runner-a",
                    scopes(&[BrokerScope::Submit, BrokerScope::Work]),
                )
                .expect("pair");
            let path = store.save().expect("save store");

            assert!(path.ends_with("broker_auth.json"));
            let loaded = BrokerAuthStore::load().expect("load store");
            loaded
                .authorize(false, Some(&minted.token), BrokerScope::Submit, None)
                .expect("submit token survives store round trip");
            loaded
                .authorize(
                    false,
                    Some(&minted.token),
                    BrokerScope::Work,
                    Some("runner-a"),
                )
                .expect("work token survives store round trip");
            assert_eq!(
                loaded.submit_token_for_runner("runner-a").as_deref(),
                Some(minted.token.as_str())
            );
        });
    }

    #[test]
    fn duplicate_active_credential_id_is_rejected() {
        let (mut store, _token) = store_with("runner-a", BrokerScope::Work);
        let err = store
            .pair("cred-1", "runner-a", scopes(&[BrokerScope::Work]))
            .expect_err("duplicate id");
        assert!(err.message.contains("already exists"));
    }
}
