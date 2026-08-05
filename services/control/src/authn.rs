//! OIDC token validation (design M12b).
//!
//! vquasar-control acts as an OAuth2 resource server: it trusts an external OIDC
//! provider (Keycloak as the reference) and validates the bearer access token on
//! each request — signature via the provider's JWKS (discovered and cached),
//! plus issuer / audience / expiry. Authentication only; authorization is RBAC
//! in [`crate::rbac`] + the store.

use std::collections::HashMap;
use std::sync::RwLock;

use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

use crate::config::AuthConfig;

/// Why a token was rejected.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid token: {0}")]
    Invalid(String),
    #[error("identity provider unreachable: {0}")]
    Provider(String),
}

/// Identity extracted from a validated token.
#[derive(Debug, Clone)]
pub struct Claims {
    pub subject: String,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub groups: Vec<String>,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
    #[serde(default)]
    alg: Option<String>,
}

#[derive(Deserialize)]
struct Discovery {
    jwks_uri: String,
}

/// Validates OIDC access tokens against a provider's JWKS.
pub struct Authenticator {
    cfg: AuthConfig,
    jwks_uri: String,
    http: reqwest::Client,
    keys: RwLock<HashMap<String, DecodingKey>>,
}

impl Authenticator {
    /// Discover the provider's JWKS endpoint and prime the key cache.
    pub async fn discover(cfg: AuthConfig) -> Result<Self, AuthError> {
        let mut builder = reqwest::Client::builder();
        // Trust an internal CA in addition to the system roots when the IdP is
        // behind one (e.g. Keycloak issued by our own CA).
        if let Some(ca_path) = &cfg.ca {
            let pem = std::fs::read(ca_path)
                .map_err(|e| AuthError::Provider(format!("reading auth CA {ca_path}: {e}")))?;
            let cert = reqwest::Certificate::from_pem(&pem)
                .map_err(|e| AuthError::Provider(format!("parsing auth CA: {e}")))?;
            builder = builder.add_root_certificate(cert);
        }
        let http = builder
            .build()
            .map_err(|e| AuthError::Provider(e.to_string()))?;
        let url = format!(
            "{}/.well-known/openid-configuration",
            cfg.issuer.trim_end_matches('/')
        );
        let disc: Discovery = http
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| AuthError::Provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| AuthError::Provider(e.to_string()))?;
        let me = Self {
            cfg,
            jwks_uri: disc.jwks_uri,
            http,
            keys: RwLock::new(HashMap::new()),
        };
        me.refresh_keys().await?;
        Ok(me)
    }

    async fn refresh_keys(&self) -> Result<(), AuthError> {
        let jwks: Jwks = self
            .http
            .get(&self.jwks_uri)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| AuthError::Provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| AuthError::Provider(e.to_string()))?;
        let mut map = HashMap::new();
        for k in jwks.keys {
            if k.alg.as_deref().is_some_and(|a| a != "RS256") {
                continue;
            }
            if let Ok(key) = DecodingKey::from_rsa_components(&k.n, &k.e) {
                map.insert(k.kid, key);
            }
        }
        *self.keys.write().unwrap() = map;
        Ok(())
    }

    fn key_for(&self, kid: &str) -> Option<DecodingKey> {
        self.keys.read().unwrap().get(kid).cloned()
    }

    /// Validate a bearer token and return the caller's identity.
    pub async fn validate(&self, token: &str) -> Result<Claims, AuthError> {
        let header = decode_header(token).map_err(|e| AuthError::Invalid(e.to_string()))?;
        let kid = header
            .kid
            .ok_or_else(|| AuthError::Invalid("no kid".into()))?;

        // Refresh once if we don't recognise the signing key (key rotation).
        let key = match self.key_for(&kid) {
            Some(k) => k,
            None => {
                self.refresh_keys().await?;
                self.key_for(&kid)
                    .ok_or_else(|| AuthError::Invalid("unknown signing key".into()))?
            }
        };

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[self.cfg.issuer.trim_end_matches('/')]);
        validation.set_audience(&[&self.cfg.audience]);
        let data = decode::<serde_json::Value>(token, &key, &validation)
            .map_err(|e| AuthError::Invalid(e.to_string()))?;
        Ok(self.claims_from(&data.claims))
    }

    fn claims_from(&self, c: &serde_json::Value) -> Claims {
        let s = |k: &str| c.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let subject = s("sub").unwrap_or_default();
        let username = s("preferred_username")
            .or_else(|| s("email"))
            .unwrap_or_else(|| subject.clone());
        let groups = c
            .get(&self.cfg.groups_claim)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|g| g.as_str())
                    // Keycloak group paths look like "/admins"; normalise.
                    .map(|g| g.trim_start_matches('/').to_string())
                    .collect()
            })
            .unwrap_or_default();
        Claims {
            subject,
            username,
            email: s("email"),
            display_name: s("name"),
            groups,
        }
    }

    pub fn config(&self) -> &AuthConfig {
        &self.cfg
    }
}
