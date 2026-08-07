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
    /// Whether requests are scoped to a project (design §47).
    pub tenancy_enabled: bool,
}

/// The header naming the project a request acts in.
///
/// A header rather than a path prefix: prefixing would double every route and
/// break every existing client and UI route, and it makes a platform admin's
/// cross-project view an awkward special case. A `?project=` query parameter is
/// accepted too, because a WebSocket handshake cannot set headers — the same
/// constraint that already forced `?access_token=` on the console.
pub const PROJECT_HEADER: &str = "x-vquasar-project";

impl AuthState {
    /// Auth disabled (dev): no authenticator configured.
    pub fn disabled() -> Self {
        Self {
            authenticator: None,
            bootstrap_admin: None,
            tenancy_enabled: false,
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

/// Generate a permission-guard extractor.
///
/// A guard validates auth **and** enforces its permission during the
/// request-parts phase — i.e. before axum deserializes the request body — so an
/// unauthorized caller gets `403` regardless of the body's shape (a bare
/// `user.require(..)` in the handler body runs only *after* the `Json` extractor,
/// which would surface a body-parse `400/422` first). Each guard wraps the
/// [`AuthUser`], so handlers that still need the caller can bind it: `Guard(user)`.
macro_rules! perm_guard {
    ($(#[$doc:meta])* $name:ident => $perm:literal) => {
        $(#[$doc])*
        pub struct $name(#[allow(dead_code)] pub AuthUser);

        #[axum::async_trait]
        impl FromRequestParts<Store> for $name {
            type Rejection = ApiError;
            async fn from_request_parts(parts: &mut Parts, store: &Store) -> Result<Self, ApiError> {
                let user = AuthUser::from_request_parts(parts, store).await?;
                user.require($perm)?;
                Ok(Self(user))
            }
        }
    };
}

perm_guard!(RequireVmCreate => "vm:create");
perm_guard!(RequireVmUpdate => "vm:update");
perm_guard!(RequireNetworkCreate => "network:create");
perm_guard!(RequireNetworkUpdate => "network:update");
perm_guard!(RequireImageCreate => "image:create");
perm_guard!(RequireImageUpdate => "image:update");
perm_guard!(RequireTemplateCreate => "template:create");
perm_guard!(RequireTemplateUpdate => "template:update");
perm_guard!(RequireHostManage => "host:manage");
perm_guard!(RequireIamManage => "iam:manage");
perm_guard!(RequireVolumeCreate => "volume:create");
perm_guard!(RequireVolumeUpdate => "volume:update");

/// The project a request acts in.
///
/// Resolution order: the `X-Vquasar-Project` header, then `?project=`, then the
/// caller's default. Absent tenancy, everything is platform scope, which is
/// what makes this a no-op for a single-tenant deployment.
///
/// A name is accepted as well as a UUID because operators type these.
pub struct RequestScope(pub vquasar_model::Scope);

#[axum::async_trait]
impl FromRequestParts<Store> for RequestScope {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, store: &Store) -> Result<Self, ApiError> {
        let auth = parts
            .extensions
            .get::<AuthState>()
            .cloned()
            .ok_or_else(|| ApiError::internal("auth state missing"))?;
        if !auth.tenancy_enabled {
            return Ok(RequestScope(vquasar_model::Scope::Platform));
        }

        let requested = parts
            .headers
            .get(PROJECT_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .or_else(|| {
                parts.uri.query().and_then(|q| {
                    q.split('&').find_map(|kv| {
                        let (k, v) = kv.split_once('=')?;
                        (k == "project").then(|| v.to_string())
                    })
                })
            });

        let Some(requested) = requested else {
            // No context: the default project, never "everything". Absence of a
            // selection must not widen what a caller can see.
            return Ok(RequestScope(vquasar_model::Scope::Project(
                vquasar_model::DEFAULT_PROJECT_ID,
            )));
        };

        let project = match uuid::Uuid::parse_str(&requested) {
            Ok(id) => store.get_project(id).await?,
            Err(_) => store
                .list_projects()
                .await?
                .into_iter()
                .find(|p| p.name == requested),
        };
        let project = project.ok_or_else(|| ApiError::not_found("project"))?;
        Ok(RequestScope(vquasar_model::Scope::Project(project.id)))
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
