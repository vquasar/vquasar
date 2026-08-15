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
use tracing::warn;

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

/// The project selector meaning "every project". Not a valid project name, so
/// it cannot collide with one.
pub const PLATFORM_SCOPE: &str = "*";

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
    /// Effective permissions **in the project this request names**.
    pub permissions: HashSet<String>,
    /// The token's group claim, kept because group bindings are half of what
    /// decides which projects the caller can act in.
    pub groups: Vec<String>,
    /// Dev mode: bypass all permission checks.
    pub superuser: bool,
}

impl AuthUser {
    /// Return `Ok` iff the caller holds `permission` (or is the dev superuser).
    ///
    /// Every 403 in the API funnels through here, which is why the log line
    /// lives here rather than at each call site: an authorization refusal that
    /// only the browser can see is one an operator cannot debug, and there are
    /// too many call sites to remember at each one.
    pub fn require(&self, permission: &str) -> Result<(), ApiError> {
        if self.superuser || self.permissions.contains(permission) {
            Ok(())
        } else {
            // The permission *and* who was refused. Either alone leaves the
            // reader guessing at the other half.
            warn!(
                user = self
                    .user
                    .as_ref()
                    .map(|u| u.username.as_str())
                    .unwrap_or("-"),
                permission, "request refused: caller does not hold this permission"
            );
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
        Ok(RequestScope(resolve_scope(parts, store).await?))
    }
}

/// Resolve (once per request) the project this request acts in.
///
/// Memoised in the request extensions because two extractors need it and the
/// name form costs a query: [`RequestScope`] uses it to pick rows, and
/// [`AuthUser`] uses it to decide which role bindings count. Those two must
/// agree — a request authorized in one project and reading another is the bug
/// this whole mechanism exists to prevent — so they read the same value rather
/// than each parsing the request again.
async fn resolve_scope(parts: &mut Parts, store: &Store) -> Result<vquasar_model::Scope, ApiError> {
    if let Some(scope) = parts.extensions.get::<vquasar_model::Scope>() {
        return Ok(*scope);
    }
    let scope = resolve_scope_uncached(parts, store).await?;
    parts.extensions.insert(scope);
    Ok(scope)
}

async fn resolve_scope_uncached(
    parts: &Parts,
    store: &Store,
) -> Result<vquasar_model::Scope, ApiError> {
    {
        let auth = parts
            .extensions
            .get::<AuthState>()
            .cloned()
            .ok_or_else(|| ApiError::internal("auth state missing"))?;
        if !auth.tenancy_enabled {
            return Ok(vquasar_model::Scope::Platform);
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

        // `*` is the platform view. It is not a privilege: permissions are
        // resolved against it too, so a caller holding only project bindings
        // resolves to the empty set here and can do nothing. It exists so a
        // platform admin has a cross-project view, and so a platform-wide role
        // binding has a scope it can be created from (ADR-020).
        if requested.as_deref() == Some(PLATFORM_SCOPE) {
            return Ok(vquasar_model::Scope::Platform);
        }

        let Some(requested) = requested else {
            // No context: the default project, never "everything". Absence of a
            // selection must not widen what a caller can see.
            return Ok(vquasar_model::Scope::Project(
                vquasar_model::DEFAULT_PROJECT_ID,
            ));
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
        Ok(vquasar_model::Scope::Project(project.id))
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
                groups: Vec::new(),
                superuser: true,
            });
        };

        // The path is worth carrying: "a token was rejected" is a puzzle,
        // "a token was rejected on /api/v1/vms" is a report.
        let path = parts.uri.path().to_string();
        let Some(token) = bearer(parts) else {
            warn!(%path, "request refused: no bearer token");
            return Err(ApiError::unauthorized("missing bearer token"));
        };
        let claims = match authenticator.validate(&token).await {
            Ok(c) => c,
            Err(e) => {
                // The reason, never the token. An expired token and a token
                // from the wrong issuer are the same 401 to the caller and
                // completely different problems to whoever has to fix it.
                warn!(%path, error = %e, "request refused: token rejected");
                return Err(ApiError::unauthorized(e.to_string()));
            }
        };

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

        // Permissions are resolved *in* the project this request names. A
        // caller with no binding there gets the empty set, so the header can be
        // anything they like and still buys them nothing (ADR-020).
        let scope = resolve_scope(parts, store).await?;
        let permissions = store
            .effective_permissions(user.id, &claims.groups, scope.project_filter())
            .await?;
        Ok(AuthUser {
            user: Some(user),
            permissions,
            groups: claims.groups,
            superuser: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::subscriber::with_default;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer;

    /// Collects the message of every event, so a test can assert on what was
    /// said rather than only on what was returned.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> Layer<S> for Captured {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Visit<'a>(&'a mut String);
            impl tracing::field::Visit for Visit<'_> {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    self.0.push_str(&format!(" {}={:?}", f.name(), v));
                }
            }
            let mut line = format!("{}", event.metadata().level());
            event.record(&mut Visit(&mut line));
            self.0.lock().unwrap().push(line);
        }
    }

    fn user(permissions: &[&str]) -> AuthUser {
        AuthUser {
            user: None,
            permissions: permissions.iter().map(|p| p.to_string()).collect(),
            groups: Vec::new(),
            superuser: false,
        }
    }

    /// The defect this exists for: a 403 answered the caller and told the
    /// operator nothing. `require` is the choke point every permission check
    /// in the API passes through, so one line here covers all of them.
    #[test]
    fn a_permission_refusal_is_logged_with_the_permission_it_wanted() {
        let cap = Captured::default();
        let sub = tracing_subscriber::registry().with(cap.clone());
        with_default(sub, || {
            assert!(user(&["vm:read"]).require("vm:delete").is_err());
        });
        let lines = cap.0.lock().unwrap().clone();
        let refusal = lines
            .iter()
            .find(|l| l.contains("does not hold this permission"))
            .unwrap_or_else(|| panic!("nothing was logged for a refused request: {lines:?}"));
        assert!(refusal.starts_with("WARN"), "{refusal}");
        // The permission that was wanted, or the reader cannot act on it.
        assert!(refusal.contains("vm:delete"), "{refusal}");
    }

    /// The quiet half: a request that is allowed must not log, or the signal
    /// drowns in a line per authorized call.
    #[test]
    fn an_allowed_request_says_nothing() {
        let cap = Captured::default();
        let sub = tracing_subscriber::registry().with(cap.clone());
        with_default(sub, || {
            assert!(user(&["vm:delete"]).require("vm:delete").is_ok());
            // …and the dev superuser, which bypasses the check entirely.
            let mut su = user(&[]);
            su.superuser = true;
            assert!(su.require("vm:delete").is_ok());
        });
        assert!(
            cap.0.lock().unwrap().is_empty(),
            "an allowed request logged: {:?}",
            cap.0.lock().unwrap()
        );
    }
}
