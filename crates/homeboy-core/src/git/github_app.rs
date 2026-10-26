//! Repository-scoped, short-lived publication credentials. Never persist this token.
use super::release_download::GitHubRepo;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GithubAppConfig {
    pub app_id: u64,
    pub private_key_path: std::path::PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<u64>,
}

/// Deliberately neither Debug nor Serialize: only the identity belongs in evidence.
pub struct PublicationCredential {
    pub identity: String,
    token: String,
}

impl PublicationCredential {
    pub fn token(&self) -> &str {
        &self.token
    }
}

fn failure(stage: &str) -> Error {
    // Do not propagate HTTP bodies, JWTs, key paths, or crypto-library errors.
    Error::internal_io(format!("GitHub App authentication failed: {stage}"), None)
}

#[derive(Serialize, Deserialize)]
struct Claims {
    iat: u64,
    exp: u64,
    iss: String,
}

fn sign(config: &GithubAppConfig, now: u64) -> Result<String> {
    let pem = std::fs::read(&config.private_key_path).map_err(|_| failure("read private key"))?;
    let key =
        jsonwebtoken::EncodingKey::from_rsa_pem(&pem).map_err(|_| failure("parse private key"))?;
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &Claims {
            iat: now.saturating_sub(60),
            exp: now + 540,
            iss: config.app_id.to_string(),
        },
        &key,
    )
    .map_err(|_| failure("sign JWT"))
}

pub fn identity(config: Option<&GithubAppConfig>) -> String {
    config
        .map(|app| format!("github_app:{}", app.app_id))
        .unwrap_or_else(|| "ambient_gh".into())
}

pub fn mint(
    config: Option<&GithubAppConfig>,
    repo: &GitHubRepo,
) -> Result<Option<PublicationCredential>> {
    let Some(config) = config else {
        return Ok(None);
    };
    let base = if repo.host == "github.com" {
        "https://api.github.com".into()
    } else {
        format!("https://{}/api/v3", repo.host)
    };
    exchange(config, repo, &base).map(Some)
}

fn exchange(
    config: &GithubAppConfig,
    repo: &GitHubRepo,
    base: &str,
) -> Result<PublicationCredential> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| failure("clock"))?
        .as_secs();
    let jwt = sign(config, now)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("homeboy")
        .build()
        .map_err(|_| failure("HTTP client"))?;
    let request = |method, url: String| {
        client
            .request(method, url)
            .bearer_auth(&jwt)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    };
    let installation = match config.installation_id {
        Some(id) => id,
        None => {
            #[derive(Deserialize)]
            struct Installation {
                id: u64,
            }
            request(
                reqwest::Method::GET,
                format!("{base}/repos/{}/{}/installation", repo.owner, repo.repo),
            )
            .send()
            .map_err(|_| failure("resolve installation"))?
            .error_for_status()
            .map_err(|_| failure("resolve installation status"))?
            .json::<Installation>()
            .map_err(|_| failure("installation response"))?
            .id
        }
    };
    #[derive(Deserialize)]
    struct AccessToken {
        token: String,
    }
    let token = request(reqwest::Method::POST, format!("{base}/app/installations/{installation}/access_tokens"))
        .json(&serde_json::json!({"repositories": [repo.repo], "permissions": {"contents": "write", "pull_requests": "write"}}))
        .send().map_err(|_| failure("exchange token"))?
        .error_for_status().map_err(|_| failure("exchange token status"))?
        .json::<AccessToken>().map_err(|_| failure("token response"))?.token;
    if token.is_empty() {
        return Err(failure("empty token"));
    }
    Ok(PublicationCredential {
        identity: identity(Some(config)),
        token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key() -> (tempfile::TempDir, GithubAppConfig) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.pem");
        assert!(std::process::Command::new("openssl")
            .args(["genrsa", "-out"])
            .arg(&path)
            .arg("2048")
            .output()
            .unwrap()
            .status
            .success());
        let config = GithubAppConfig {
            app_id: 42,
            private_key_path: path,
            installation_id: None,
        };
        (dir, config)
    }
    #[test]
    fn jwt_is_rs256_signed_with_bounded_claims() {
        let (_dir, config) = key();
        let jwt = sign(&config, 1_000).unwrap();
        let public = std::process::Command::new("openssl")
            .args(["rsa", "-pubout", "-in"])
            .arg(&config.private_key_path)
            .output()
            .unwrap();
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.validate_exp = false;
        let claims = jsonwebtoken::decode::<Claims>(
            &jwt,
            &jsonwebtoken::DecodingKey::from_rsa_pem(&public.stdout).unwrap(),
            &validation,
        )
        .unwrap()
        .claims;
        assert_eq!(claims.iat, 940);
        assert_eq!(claims.exp, 1540);
        assert_eq!(claims.iss, "42");
    }
    #[test]
    fn exchange_resolves_installation_and_scopes_token() {
        use std::io::{Read, Write};
        let (_dir, config) = key();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for (path, response) in [
                ("GET /repos/owner/repo/installation ", r#"{"id":123}"#),
                (
                    "POST /app/installations/123/access_tokens ",
                    r#"{"token":"test-secret-token"}"#,
                ),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut byte = [0u8; 1];
                while !bytes.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                let headers = String::from_utf8(bytes).unwrap();
                assert!(headers.starts_with(path));
                assert!(headers.to_lowercase().contains("authorization: bearer ey"));
                assert!(headers
                    .to_lowercase()
                    .contains("accept: application/vnd.github+json"));
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|s| s.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                if path.starts_with("POST") {
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                        serde_json::json!({
                            "repositories": ["repo"], "permissions": {"contents":"write", "pull_requests":"write"}
                        })
                    );
                }
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
            }
        });
        let repo = GitHubRepo {
            host: "github.com".into(),
            owner: "owner".into(),
            repo: "repo".into(),
        };
        let credential = exchange(&config, &repo, &base).unwrap();
        assert_eq!(credential.token(), "test-secret-token");
        assert_eq!(credential.identity, "github_app:42");
        assert!(!credential.identity.contains(credential.token()));
        server.join().unwrap();
    }

    #[test]
    fn failures_do_not_include_key_path() {
        let config = GithubAppConfig {
            app_id: 42,
            private_key_path: "/secret/key.pem".into(),
            installation_id: None,
        };
        let error = sign(&config, 1000).err().unwrap();
        assert!(!format!("{error:?}").contains("/secret"));
    }

    #[test]
    fn fallback_needs_no_key_or_network() {
        let repo = GitHubRepo {
            host: "github.com".into(),
            owner: "owner".into(),
            repo: "repo".into(),
        };
        assert!(mint(None, &repo).unwrap().is_none());
        assert_eq!(identity(None), "ambient_gh");
    }
}
