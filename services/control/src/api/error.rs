//! Public API error envelope (design document, section 37).
//!
//! Internal errors (sqlx, agent transport, ...) are never leaked verbatim; they
//! collapse to a stable [`ErrorCode`] and a safe message, with a `request_id`
//! for correlation.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use uuid::Uuid;
use vquasar_common::ErrorCode;

/// An error renderable as the public JSON envelope.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: ErrorCode,
    message: String,
}

impl ApiError {
    pub fn vm_not_found(id: Uuid) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: ErrorCode::VmNotFound,
            message: format!("virtual machine not found: {id}"),
        }
    }

    pub fn host_not_found(id: Uuid) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: ErrorCode::HostUnavailable,
            message: format!("host not found: {id}"),
        }
    }

    pub fn task_not_found(id: Uuid) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: ErrorCode::Internal,
            message: format!("task not found: {id}"),
        }
    }

    /// No such route under `/api/v1`. Distinct from a missing resource: it
    /// keeps an unknown API path on the JSON envelope instead of letting it
    /// fall through to the single-page shell.
    pub fn route_not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: ErrorCode::Internal,
            message: "no such API route".into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: ErrorCode::InvalidConfiguration,
            message: message.into(),
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: ErrorCode::Unauthorized,
            message: message.into(),
        }
    }

    pub fn forbidden(permission: &str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: ErrorCode::Forbidden,
            message: format!("missing permission: {permission}"),
        }
    }

    /// A resource that does not exist, or that the caller may not see.
    ///
    /// Deliberately says only what kind of thing was not found: distinguishing
    /// "no such id" from "not yours" is an existence oracle once projects
    /// scope things, and the difference is not useful to a legitimate caller.
    pub fn not_found(kind: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: ErrorCode::NotFound,
            message: format!("{kind} not found"),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: ErrorCode::Internal,
            message: message.into(),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        // sqlx errors carry table, column and constraint names, and sometimes
        // the offending value — a unique-violation message is an existence
        // oracle for a row the caller may not be allowed to know about. Log the
        // detail for operators and return a constant message, as the module
        // doc promises (design §37).
        tracing::error!(error = %e, "database error");
        ApiError::internal("database error")
    }
}

#[derive(Serialize)]
struct Envelope {
    error: Body,
}

#[derive(Serialize)]
struct Body {
    code: ErrorCode,
    message: String,
    request_id: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Envelope {
            error: Body {
                code: self.code,
                message: self.message,
                request_id: Uuid::new_v4().to_string(),
            },
        };
        (self.status, Json(body)).into_response()
    }
}

/// Handler result alias.
pub type ApiResult<T> = std::result::Result<T, ApiError>;
