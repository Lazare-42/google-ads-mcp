use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use jsonwebtoken::{Algorithm, EncodingKey, Header as JwtHeader};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::{Error, Result};

const SCOPE: &str = "https://www.googleapis.com/auth/adwords";
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const REFRESH_SKEW_SECS: u64 = 60;

#[derive(Clone, Deserialize)]
struct ServiceAccountKey {
    client_email: String,
    private_key: String,
    #[serde(default)]
    private_key_id: Option<String>,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    DEFAULT_TOKEN_URI.to_string()
}

#[derive(Debug, Serialize)]
struct Claims {
    iss: String,
    scope: String,
    aud: String,
    iat: u64,
    exp: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Clone)]
struct CachedToken {
    access_token: String,
    refresh_at: u64,
}

pub struct GoogleAuth {
    http: Client,
    key: ServiceAccountKey,
    cached: RwLock<Option<CachedToken>>,
}

impl GoogleAuth {
    pub fn from_env() -> Result<Arc<Self>> {
        let path = std::env::var("GOOGLE_APPLICATION_CREDENTIALS").map_err(|_| {
            Error::Auth(
                "GOOGLE_APPLICATION_CREDENTIALS is not set (service-account JSON path)".into(),
            )
        })?;
        let bytes = std::fs::read(&path).map_err(|e| {
            Error::Auth(format!(
                "failed to read GOOGLE_APPLICATION_CREDENTIALS at {path}: {e}"
            ))
        })?;
        let key: ServiceAccountKey = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Auth(format!("failed to parse service-account JSON: {e}")))?;
        let http = Client::builder()
            .user_agent(concat!("google-ads-mcp/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;

        Ok(Arc::new(Self {
            http,
            key,
            cached: RwLock::new(None),
        }))
    }

    pub fn service_account_email(&self) -> &str {
        &self.key.client_email
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn build_assertion(&self) -> Result<String> {
        let now = Self::now_secs();
        let claims = Claims {
            iss: self.key.client_email.clone(),
            scope: SCOPE.to_string(),
            aud: self.key.token_uri.clone(),
            iat: now,
            exp: now + 3600,
        };
        let mut header = JwtHeader::new(Algorithm::RS256);
        header.kid = self.key.private_key_id.clone();
        let key = EncodingKey::from_rsa_pem(self.key.private_key.as_bytes())
            .map_err(|e| Error::Auth(format!("invalid service-account private key: {e}")))?;
        jsonwebtoken::encode(&header, &claims, &key)
            .map_err(|e| Error::Auth(format!("failed to sign JWT assertion: {e}")))
    }

    async fn fetch_token(&self) -> Result<CachedToken> {
        let assertion = self.build_assertion()?;
        let response = self
            .http
            .post(&self.key.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(Error::Auth(format!("token endpoint returned {status}")));
        }
        // Never include the OAuth response body in an error. A malformed successful
        // response could still contain a usable access token.
        let token: TokenResponse = serde_json::from_str(&body)
            .map_err(|e| Error::Auth(format!("failed to parse token response: {e}")))?;
        Ok(CachedToken {
            access_token: token.access_token,
            refresh_at: Self::now_secs() + token.expires_in.saturating_sub(REFRESH_SKEW_SECS),
        })
    }

    pub async fn access_token(&self) -> Result<String> {
        {
            let guard = self.cached.read().await;
            if let Some(token) = guard.as_ref() {
                if Self::now_secs() < token.refresh_at {
                    return Ok(token.access_token.clone());
                }
            }
        }

        let mut guard = self.cached.write().await;
        if let Some(token) = guard.as_ref() {
            if Self::now_secs() < token.refresh_at {
                return Ok(token.access_token.clone());
            }
        }
        let token = self.fetch_token().await?;
        let access_token = token.access_token.clone();
        *guard = Some(token);
        Ok(access_token)
    }
}
