//! RBAC catalog and built-in roles (design M12b).
//!
//! Permissions are `resource:action` strings. The catalog here is the single
//! source of truth; custom roles may only grant permissions from it, and the
//! built-in roles are re-synced from these definitions on every startup.

/// Every permission the platform recognises.
pub const CATALOG: &[&str] = &[
    "vm:create",
    "vm:read",
    "vm:update",
    "vm:delete",
    "vm:power",
    "vm:migrate",
    "vm:console",
    "host:read",
    "host:manage",
    "network:create",
    "network:read",
    "network:update",
    "network:delete",
    "volume:create",
    "volume:read",
    "volume:update",
    "volume:delete",
    "image:create",
    "image:read",
    "image:update",
    "image:delete",
    "template:create",
    "template:read",
    "template:update",
    "template:delete",
    "iam:read",
    "iam:manage",
];

/// Whether `perm` is part of the catalog (rejects typos in custom roles).
pub fn is_valid(perm: &str) -> bool {
    CATALOG.contains(&perm)
}

/// A built-in role: name, description, and the permissions it grants.
pub struct BuiltinRole {
    pub name: &'static str,
    pub description: &'static str,
    pub permissions: Vec<&'static str>,
}

fn ends_with_read(p: &str) -> bool {
    p.ends_with(":read")
}

/// The seeded roles, re-synced from code on startup.
pub fn builtin_roles() -> Vec<BuiltinRole> {
    let all: Vec<&str> = CATALOG.to_vec();

    // operator: manage workloads and their resources, but not identity or hosts.
    let operator: Vec<&str> = CATALOG
        .iter()
        .copied()
        .filter(|p| !p.starts_with("iam:") && *p != "host:manage")
        .collect();

    // viewer: read-only across resources, plus console access.
    let mut viewer: Vec<&str> = CATALOG
        .iter()
        .copied()
        .filter(|p| ends_with_read(p))
        .collect();
    viewer.retain(|p| *p != "iam:read");
    viewer.push("vm:console");

    vec![
        BuiltinRole {
            name: "admin",
            description: "Full access to everything, including identity & roles.",
            permissions: all,
        },
        BuiltinRole {
            name: "operator",
            description: "Manage VMs, networks, images and templates; read hosts.",
            permissions: operator,
        },
        BuiltinRole {
            name: "viewer",
            description: "Read-only access, plus VM console.",
            permissions: viewer,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for p in CATALOG {
            assert!(seen.insert(*p), "duplicate permission {p}");
            assert!(p.contains(':'), "permission {p} must be resource:action");
        }
    }

    #[test]
    fn builtin_permissions_are_all_in_catalog() {
        for role in builtin_roles() {
            for p in &role.permissions {
                assert!(is_valid(p), "role {} grants unknown perm {p}", role.name);
            }
        }
    }

    #[test]
    fn role_shapes() {
        let roles = builtin_roles();
        let admin = roles.iter().find(|r| r.name == "admin").unwrap();
        assert_eq!(admin.permissions.len(), CATALOG.len());
        let operator = roles.iter().find(|r| r.name == "operator").unwrap();
        assert!(!operator.permissions.contains(&"iam:manage"));
        assert!(!operator.permissions.contains(&"host:manage"));
        assert!(operator.permissions.contains(&"vm:create"));
        let viewer = roles.iter().find(|r| r.name == "viewer").unwrap();
        assert!(viewer.permissions.contains(&"vm:console"));
        assert!(viewer.permissions.contains(&"vm:read"));
        assert!(!viewer.permissions.contains(&"vm:delete"));
    }
}
