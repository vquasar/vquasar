//! Identity & RBAC endpoints (design M12b): current-user info, the permission
//! catalog, and management of users, roles, and group→role mappings.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::{AuthState, AuthUser, RequireIamManage};
use crate::rbac;
use crate::store::{Role, Store, User};

/// Public: what the SPA needs to start an OIDC login.
#[derive(Serialize)]
pub struct AuthConfigView {
    pub enabled: bool,
    pub issuer: String,
    pub client_id: String,
}

pub async fn auth_config(Extension(auth): Extension<AuthState>) -> Json<AuthConfigView> {
    match auth.authenticator {
        Some(a) => {
            let c = a.config();
            Json(AuthConfigView {
                enabled: true,
                issuer: c.issuer.clone(),
                client_id: c.client_id.clone(),
            })
        }
        None => Json(AuthConfigView {
            enabled: false,
            issuer: String::new(),
            client_id: String::new(),
        }),
    }
}

#[derive(Serialize)]
pub struct Me {
    pub authenticated: bool,
    pub username: Option<String>,
    pub email: Option<String>,
    /// Effective permissions **in `project`** — the same resolution the guards
    /// use, so the UI hides exactly what the API would refuse.
    pub permissions: Vec<String>,
    /// The project this answer is about; absent means the platform view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<Uuid>,
    /// Whether requests are project-scoped at all. The UI needs this to decide
    /// whether to show a project selector; inferring it from `project` being
    /// absent would conflate "tenancy is off" with "you are in the platform
    /// view", which are different answers to a different question.
    pub tenancy: bool,
    /// Whether the caller holds a platform-wide binding, and so may take the
    /// cross-project view. Reported rather than discovered by trying, so the UI
    /// does not have to offer an option that answers 403.
    pub platform: bool,
}

/// The current caller's identity + effective permissions (drives the UI).
pub async fn me(
    State(store): State<Store>,
    Extension(auth): Extension<AuthState>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
) -> ApiResult<Json<Me>> {
    let mut permissions: Vec<String> = if user.superuser {
        rbac::CATALOG.iter().map(|s| s.to_string()).collect()
    } else {
        user.permissions.iter().cloned().collect()
    };
    permissions.sort();
    let platform = match user.user.as_ref() {
        // Dev superuser: no bindings exist, and everything is permitted.
        None => true,
        Some(u) => store
            .projects_for_caller(u.id, &user.groups)
            .await?
            .is_none(),
    };
    Ok(Json(Me {
        authenticated: user.user.is_some() || user.superuser,
        username: user.user.as_ref().map(|u| u.username.clone()),
        email: user.user.as_ref().and_then(|u| u.email.clone()),
        permissions,
        project: scope.0.project_filter(),
        tenancy: auth.tenancy_enabled,
        platform,
    }))
}

/// The full permission catalog (for building custom roles).
pub async fn permissions(user: AuthUser) -> ApiResult<Json<Vec<&'static str>>> {
    user.require("iam:read")?;
    Ok(Json(rbac::CATALOG.to_vec()))
}

#[derive(Serialize)]
pub struct UserView {
    #[serde(flatten)]
    pub user: User,
    pub roles: Vec<Role>,
}

pub async fn list_users(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
) -> ApiResult<Json<Vec<UserView>>> {
    user.require("iam:read")?;
    // Roles are reported for the scope the request is acting in — the same
    // bindings `set_user_roles` would replace from here (ADR-020).
    let mut out = Vec::new();
    for u in store.list_users().await? {
        let roles = store.roles_for_user(u.id, scope.0.project_filter()).await?;
        out.push(UserView { user: u, roles });
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct SetUserRoles {
    pub role_ids: Vec<Uuid>,
}

pub async fn set_user_roles(
    State(store): State<Store>,
    _: RequireIamManage,
    scope: crate::authz::RequestScope,
    Path(id): Path<Uuid>,
    Json(body): Json<SetUserRoles>,
) -> ApiResult<Json<UserView>> {
    let target = store
        .get_user(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("user not found: {id}")))?;
    // The binding is made in the scope the caller is acting in — which is the
    // scope their own `iam:manage` was resolved in. A project admin therefore
    // cannot mint a platform-wide grant: reaching platform scope needs
    // `X-Vquasar-Project: *`, where their permissions resolve to nothing.
    let project = scope.0.project_filter();
    store.set_user_roles(id, &body.role_ids, project).await?;
    let roles = store.roles_for_user(id, project).await?;
    Ok(Json(UserView {
        user: target,
        roles,
    }))
}

#[derive(Serialize)]
pub struct RoleView {
    #[serde(flatten)]
    pub role: Role,
    pub permissions: Vec<String>,
}

pub async fn list_roles(
    State(store): State<Store>,
    user: AuthUser,
) -> ApiResult<Json<Vec<RoleView>>> {
    user.require("iam:read")?;
    let mut out = Vec::new();
    for r in store.list_roles().await? {
        let permissions = store.role_permissions(r.id).await?;
        out.push(RoleView {
            role: r,
            permissions,
        });
    }
    Ok(Json(out))
}

pub async fn get_role(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RoleView>> {
    user.require("iam:read")?;
    let role = store
        .get_role(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("role not found: {id}")))?;
    let permissions = store.role_permissions(id).await?;
    Ok(Json(RoleView { role, permissions }))
}

#[derive(Deserialize)]
pub struct CreateRole {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

fn validate_perms(perms: &[String]) -> ApiResult<()> {
    for p in perms {
        if !rbac::is_valid(p) {
            return Err(ApiError::invalid(format!("unknown permission: {p}")));
        }
    }
    Ok(())
}

pub async fn create_role(
    State(store): State<Store>,
    _: RequireIamManage,
    Json(body): Json<CreateRole>,
) -> ApiResult<Json<RoleView>> {
    if body.name.is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    validate_perms(&body.permissions)?;
    let role = store
        .create_role(&body.name, body.description.as_deref(), &body.permissions)
        .await?;
    Ok(Json(RoleView {
        role,
        permissions: body.permissions,
    }))
}

#[derive(Deserialize)]
pub struct UpdateRole {
    #[serde(default)]
    pub description: Option<String>,
    pub permissions: Vec<String>,
}

pub async fn update_role(
    State(store): State<Store>,
    _: RequireIamManage,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateRole>,
) -> ApiResult<Json<RoleView>> {
    let role = store
        .get_role(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("role not found: {id}")))?;
    if role.builtin {
        return Err(ApiError::invalid("built-in roles cannot be edited"));
    }
    validate_perms(&body.permissions)?;
    store
        .update_role_permissions(id, body.description.as_deref(), &body.permissions)
        .await?;
    Ok(Json(RoleView {
        role,
        permissions: body.permissions,
    }))
}

pub async fn delete_role(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::http::StatusCode> {
    user.require("iam:manage")?;
    if store.delete_role(id).await? {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::invalid("role not found or is built-in"))
    }
}

#[derive(Serialize)]
pub struct GroupRoleView {
    pub group: String,
    pub role: String,
    /// The project the mapping applies in; absent means platform-wide.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Uuid>,
}

pub async fn list_group_roles(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
) -> ApiResult<Json<Vec<GroupRoleView>>> {
    user.require("iam:read")?;
    let rows = store.list_group_roles(scope.0.project_filter()).await?;
    Ok(Json(
        rows.into_iter()
            .map(|(group, role, project_id)| GroupRoleView {
                group,
                role,
                project_id,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct AddGroupRole {
    pub group: String,
    pub role_id: Uuid,
}

pub async fn add_group_role(
    State(store): State<Store>,
    _: RequireIamManage,
    scope: crate::authz::RequestScope,
    Json(body): Json<AddGroupRole>,
) -> ApiResult<axum::http::StatusCode> {
    if body.group.is_empty() {
        return Err(ApiError::invalid("group is required"));
    }
    store
        .add_group_role(&body.group, body.role_id, scope.0.project_filter())
        .await?;
    Ok(axum::http::StatusCode::CREATED)
}

pub async fn remove_group_role(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
    Path((group, role_id)): Path<(String, Uuid)>,
) -> ApiResult<axum::http::StatusCode> {
    user.require("iam:manage")?;
    if store
        .remove_group_role(&group, role_id, scope.0.project_filter())
        .await?
    {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::invalid("mapping not found"))
    }
}
