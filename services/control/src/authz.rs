//! Request authorization (design M12b).
//!
//! [`AuthUser`] is an axum extractor: it validates the bearer token (via the
//! configured [`Authenticator`]), JIT-provisions the local user, applies the
//! first-admin bootstrap, and loads the caller's effective permissions. Handlers
//! then call [`AuthUser::require`] to gate on a specific permission.
//!
//! When auth is not configured (dev escape hatch) the extractor yields a
//! `superuser` that passes every check, so the platform stays usable until an
//! identity provider is wired up.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::api::error::ApiError;
use crate::authn::Authenticator;
use crate::store::{Store, User};

/// Shared auth wiring, attached to the router as an extension.
#[derive(Clone)]
pub struct AuthState {
    pub authenticator: Option<Arc<Authenticator>>,
    pub bootstrap_admin: Option<String>,
}

impl AuthState {
    /// Auth disabled (dev): no authenticator configured.
    pub fn disabled() -> Self {
        Self {
            authenticator: None,
            bootstrap_admin: None,
        }
    }
}

/// The authenticated caller and their effective permissions.
pub struct AuthUser {
    pub user: Option<User>,
    pub permissions: HashSet<String>,
    /// Dev mode: bypass all permission checks.
    pub superuser: bool,
}

impl AuthUser {
    /// Return `Ok` iff the caller holds `permission` (or is the dev superuser).
    pub fn require(&self, permission: &str) -> Result<(), ApiError> {
        if self.superuser || self.permissions.contains(permission) {
            Ok(())
        } else {
            Err(ApiError::forbidden(permission))
        }
    }
}

fn bearer(parts: &Parts) -> Option<String> {
    let h = parts.headers.get(axum::http::header::AUTHORIZATION)?;
    let s = h.to_str().ok()?;
    s.strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
        .map(str::to_string)
}

#[axum::async_trait]
impl FromRequestParts<Store> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, store: &Store) -> Result<Self, ApiError> {
        let auth = parts
            .extensions
            .get::<AuthState>()
            .cloned()
            .ok_or_else(|| ApiError::internal("auth state missing"))?;

        // Dev escape hatch: no IdP configured -> everyone is a superuser.
        let Some(authenticator) = auth.authenticator else {
            return Ok(AuthUser {
                user: None,
                permissions: HashSet::new(),
                superuser: true,
            });
        };

        let token = bearer(parts).ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
        let claims = authenticator
            .validate(&token)
            .await
            .map_err(|e| ApiError::unauthorized(e.to_string()))?;

        // Mirror the identity locally (JIT) so roles can attach to it.
        let user = store
            .upsert_user(
                &claims.subject,
                &claims.username,
                claims.email.as_deref(),
                claims.display_name.as_deref(),
            )
            .await?;

        // First-admin bootstrap: grant admin to the configured identity.
        if let Some(ba) = &auth.bootstrap_admin {
            let matches = ba == &claims.subject
                || ba == &claims.username
                || claims.email.as_deref() == Some(ba.as_str());
            if matches {
                let _ = store.grant_role_by_name(user.id, "admin").await;
            }
        }

        let permissions = store.effective_permissions(user.id, &claims.groups).await?;
        Ok(AuthUser {
            user: Some(user),
            permissions,
            superuser: false,
        })
    }
}
