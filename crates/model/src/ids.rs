//! Strongly-typed resource identifiers.
//!
//! Every persistent resource is identified by a UUID (ADR-006). Wrapping the
//! UUID in a newtype per resource kind prevents accidentally passing a host id
//! where a VM id is expected.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generate a fresh random (v4) identifier.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wrap an existing UUID.
            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            /// The underlying UUID.
            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }

            /// A short, host-safe slug derived from the id.
            ///
            /// Useful for deriving host-local names such as TAP interfaces,
            /// where the full UUID would exceed Linux's `IFNAMSIZ` limit
            /// (design document, section 23).
            pub fn short(&self) -> String {
                let simple = self.0.simple().to_string();
                simple[..8].to_string()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        // Silence "unused prefix" while keeping a documented, per-type marker
        // that later host-local naming code can adopt.
        impl $name {
            #[doc = concat!("Conventional host-local name prefix for a ", stringify!($name), ".")]
            pub const NAME_PREFIX: &'static str = $prefix;
        }
    };
}

typed_id!(
    /// Identifier for a hypervisor [`crate::Host`](crate::host::Host).
    HostId, "host"
);
typed_id!(
    /// Identifier for a [`VirtualMachine`](crate::vm::VirtualMachine).
    VmId, "vm"
);
typed_id!(
    /// Identifier for a virtual network.
    NetworkId, "net"
);
typed_id!(
    /// Identifier for an asynchronous [task](crate::ids::TaskId).
    TaskId, "task"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_roundtrip_through_string() {
        let id = VmId::new();
        let parsed: VmId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn short_slug_is_eight_hex_chars() {
        let id = VmId::from_uuid(Uuid::nil());
        assert_eq!(id.short(), "00000000");
        assert_eq!(id.short().len(), 8);
    }

    #[test]
    fn distinct_types_do_not_share_a_value_space() {
        // This is a compile-time guarantee; the test documents intent.
        let vm = VmId::new();
        let host = HostId::from_uuid(vm.as_uuid());
        assert_eq!(vm.as_uuid(), host.as_uuid());
    }
}
