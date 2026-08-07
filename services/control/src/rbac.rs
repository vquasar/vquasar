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
    // Attaching a network to physical infrastructure (an uplink, a VLAN tag) is
    // a platform decision: the tag determines which provider segment you land
    // on. Held by `admin` only — deliberately not by `operator` (ADR-016).
    "network:create:provider",
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
    // Tenancy boundaries are platform objects: creating or deleting one is not
    // a workload operation, and `operator` deliberately holds none of these
    // beyond reading (design §47, ADR-018).
    "project:read",
    "project:create",
    "project:update",
    "project:delete",
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

    // operator: manage workloads and their resources, but not identity, hosts,
    // or attachment to physical infrastructure.
    let operator: Vec<&str> = CATALOG
        .iter()
        .copied()
        .filter(|p| {
            !p.starts_with("iam:")
                && *p != "host:manage"
                && *p != "network:create:provider"
                // Reading the project list is fine; shaping tenancy is not.
                && !matches!(*p, "project:create" | "project:update" | "project:delete")
        })
        .collect();

    // viewer: read-only across resources.
    //
    // Deliberately *not* vm:console. A serial console is an interactive,
    // root-adjacent session on the guest — whatever is logged in there, the
    // holder can drive. Calling that "read-only" was wrong: it made the most
    // restricted role the one with the widest practical reach.
    let mut viewer: Vec<&str> = CATALOG
        .iter()
        .copied()
        .filter(|p| ends_with_read(p))
        .collect();
    viewer.retain(|p| *p != "iam:read");

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
        // Physical attachment is platform-only (ADR-016).
        assert!(!operator.permissions.contains(&"network:create:provider"));
        // Tenancy boundaries are platform-shaped, not workload-shaped.
        assert!(!operator.permissions.contains(&"project:create"));
        assert!(!operator.permissions.contains(&"project:delete"));
        assert!(operator.permissions.contains(&"project:read"));
        assert!(operator.permissions.contains(&"network:create"));
        let admin = roles.iter().find(|r| r.name == "admin").unwrap();
        assert!(admin.permissions.contains(&"network:create:provider"));
        assert!(operator.permissions.contains(&"vm:create"));
        let viewer = roles.iter().find(|r| r.name == "viewer").unwrap();
        // A console is interactive access to the guest, not a read.
        assert!(!viewer.permissions.contains(&"vm:console"));
        let operator = roles.iter().find(|r| r.name == "operator").unwrap();
        assert!(operator.permissions.contains(&"vm:console"));
        assert!(viewer.permissions.contains(&"vm:read"));
        assert!(!viewer.permissions.contains(&"vm:delete"));
    }
}
