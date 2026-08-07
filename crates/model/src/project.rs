//! Projects — the unit of tenancy (design §47, ADR-018).
//!
//! A project owns VMs, volumes, templates and security groups. It does *not*
//! own hosts, users, role definitions or the CA: those are platform resources,
//! and exposing a host inventory to a tenant is a leak with no tenant benefit.
//!
//! Images and networks sit in between. They are *shareable*: an unset project
//! means platform-shared and usable by everyone, which is what lets a fleet's
//! curated images and its provider network keep working the moment a second
//! project appears.

use serde::{Deserialize, Serialize};

/// The project every pre-tenancy resource belongs to.
///
/// A fixed id rather than a lookup so migrations, backfills and operator
/// queries can all name the same row without a join.
pub const DEFAULT_PROJECT_ID: uuid::Uuid = uuid::uuid!("00000000-0000-0000-0000-000000000001");

/// Who a resource belongs to, from the perspective of a request.
///
/// The distinction is deliberate rather than an `Option<Uuid>`: "no project"
/// and "every project" are opposite answers, and a nullable id invites code
/// that treats absence as permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// A single project's resources, and the shared catalogues.
    Project(uuid::Uuid),
    /// Everything, for the reconcile loop and platform administrators.
    Platform,
}

impl Scope {
    /// The project id to bind into a query, or `None` for platform scope.
    ///
    /// Every scoped query uses the same predicate shape —
    /// `($n::uuid IS NULL OR project_id = $n)` — so platform scope is a bound
    /// NULL rather than a different statement. One query, one plan, no string
    /// assembly.
    pub fn project_filter(self) -> Option<uuid::Uuid> {
        match self {
            Scope::Project(id) => Some(id),
            Scope::Platform => None,
        }
    }

    pub fn is_platform(self) -> bool {
        matches!(self, Scope::Platform)
    }
}

/// A tenancy boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: uuid::Uuid,
    pub name: String,
    pub description: Option<String>,
    /// The fallback project. Cannot be deleted: every caller without project
    /// context resolves here, so removing it would strand them.
    pub is_default: bool,
}

/// Why a project name was rejected.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProjectValidationError {
    #[error("project name must not be empty")]
    EmptyName,
    #[error("project name must be at most {max} characters, got {got}")]
    NameTooLong { got: usize, max: usize },
    #[error(
        "project name may contain only lowercase letters, digits and hyphens, \
         and must start with a letter or digit"
    )]
    InvalidName,
}

/// Longer than any sensible project name, short enough to render in a table.
const MAX_NAME: usize = 63;

/// Validate a project name.
///
/// Constrained rather than free-form because a project name is an identifier
/// people type, put in URLs and read in logs — the same reasoning as a DNS
/// label, and the same character set.
pub fn validate_name(name: &str) -> Result<(), ProjectValidationError> {
    if name.is_empty() {
        return Err(ProjectValidationError::EmptyName);
    }
    if name.len() > MAX_NAME {
        return Err(ProjectValidationError::NameTooLong {
            got: name.len(),
            max: MAX_NAME,
        });
    }
    let first_ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !first_ok || !rest_ok {
        return Err(ProjectValidationError::InvalidName);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Platform scope must bind NULL, not a sentinel id — the predicate relies
    /// on it, and a sentinel would silently match a real project one day.
    #[test]
    fn platform_scope_filters_nothing() {
        assert_eq!(Scope::Platform.project_filter(), None);
        assert!(Scope::Platform.is_platform());
        let p = uuid::Uuid::new_v4();
        assert_eq!(Scope::Project(p).project_filter(), Some(p));
        assert!(!Scope::Project(p).is_platform());
    }

    #[test]
    fn the_default_project_id_is_fixed() {
        assert_eq!(
            DEFAULT_PROJECT_ID.to_string(),
            "00000000-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn names_are_identifier_shaped() {
        assert!(validate_name("platform").is_ok());
        assert!(validate_name("team-blue").is_ok());
        assert!(validate_name("t2").is_ok());
        assert_eq!(validate_name(""), Err(ProjectValidationError::EmptyName));
        assert_eq!(
            validate_name("Team Blue"),
            Err(ProjectValidationError::InvalidName)
        );
        assert_eq!(
            validate_name("-leading"),
            Err(ProjectValidationError::InvalidName)
        );
        assert_eq!(
            validate_name("under_score"),
            Err(ProjectValidationError::InvalidName)
        );
        assert!(matches!(
            validate_name(&"a".repeat(64)),
            Err(ProjectValidationError::NameTooLong { .. })
        ));
    }
}
