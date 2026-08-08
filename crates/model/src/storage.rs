//! Storage pools: where a volume's bytes live, and who can reach them
//! (design §20, ADR-023).
//!
//! Storage used to be one configured directory that every host was *assumed* to
//! have mounted at the same path. Two separate things were tangled in that one
//! value — *where bytes go* and *which hosts can reach them* — and neither was
//! recorded, so a host without the mount failed at launch instead of being
//! refused at placement.
//!
//! A [`PoolParams`] says where bytes go. It deliberately says nothing about
//! reachability or capacity: both are *observed* from the agents (ADR-023),
//! because an operator declaring either records an intention the filesystem is
//! free to contradict.

use serde::{Deserialize, Serialize};

/// What kind of storage a pool is, and therefore how bytes are placed in it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoragePoolKind {
    /// A directory the hosts that report it have mounted at the same path.
    ///
    /// This is what live migration has always depended on, now written down.
    #[default]
    SharedDir,
}

impl StoragePoolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StoragePoolKind::SharedDir => "shared_dir",
        }
    }
}

impl std::str::FromStr for StoragePoolKind {
    type Err = InvalidStoragePoolKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "shared_dir" => Ok(StoragePoolKind::SharedDir),
            other => Err(InvalidStoragePoolKind(other.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown storage pool kind {0:?} — expected shared_dir")]
pub struct InvalidStoragePoolKind(pub String);

/// A pool's kind together with the parameters that kind needs.
///
/// Serialised internally tagged, so the stored JSON carries its own kind and a
/// row can never be read as the wrong shape. `lvm_thin`, `nfs` and `rbd` are
/// the shapes this was built to accept: each becomes one more variant, and
/// nothing that reads a pool has to change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PoolParams {
    SharedDir { path: String },
}

impl PoolParams {
    pub fn kind(&self) -> StoragePoolKind {
        match self {
            PoolParams::SharedDir { .. } => StoragePoolKind::SharedDir,
        }
    }

    /// The pool's root as the agents see it, when the kind has one.
    ///
    /// A future networked kind (`rbd`, `nfs` before it is mounted) has no host
    /// path, which is why this is an `Option` rather than a field.
    pub fn host_path(&self) -> Option<&str> {
        match self {
            PoolParams::SharedDir { path } => Some(path),
        }
    }

    /// Check the parameters are coherent on their own terms.
    ///
    /// Confinement to the platform's storage roots is *not* checked here: the
    /// permitted roots are control-plane configuration, so that check lives
    /// with the API alongside the one for VM disk paths (design §30).
    pub fn validate(&self) -> Result<(), PoolValidationError> {
        match self {
            PoolParams::SharedDir { path } => {
                if path.trim().is_empty() {
                    return Err(PoolValidationError::MissingPath);
                }
                if !path.starts_with('/') {
                    return Err(PoolValidationError::RelativePath(path.clone()));
                }
                Ok(())
            }
        }
    }
}

/// Why a proposed pool is not coherent.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PoolValidationError {
    #[error("a shared_dir pool requires a path")]
    MissingPath,
    #[error("path must be absolute, got {0:?}")]
    RelativePath(String),
    #[error("name must not be empty")]
    EmptyName,
    #[error("name must be at most 63 characters, got {0}")]
    NameTooLong(usize),
    #[error(
        "name may contain only lowercase letters, digits and '-', \
         and must start and end with a letter or digit: {0:?}"
    )]
    InvalidName(String),
}

/// Check a pool name.
///
/// A pool is named in operator-facing places — a volume says which pool it is
/// in, an agent reports pools by name — so the name is kept to the same
/// conservative shape as everything else that ends up in a config file or a
/// path component.
pub fn validate_pool_name(name: &str) -> Result<(), PoolValidationError> {
    if name.is_empty() {
        return Err(PoolValidationError::EmptyName);
    }
    if name.len() > 63 {
        return Err(PoolValidationError::NameTooLong(name.len()));
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !ok {
        return Err(PoolValidationError::InvalidName(name.to_string()));
    }
    Ok(())
}

/// Whether a pool is usable, which is a fact about the fleet rather than about
/// the row.
///
/// The distinction hosts have, for the same reason: a pool no host reports is
/// not usable however correct its configuration looks. It is reported as such
/// rather than hidden, because the usual cause is a mount that has not come
/// back yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoragePoolState {
    /// No host has reported it. Configured, not yet known to work anywhere.
    Pending,
    /// At least one host reports it can use this pool.
    Ready,
}

impl StoragePoolState {
    /// Derive the state from the number of hosts currently reporting the pool.
    pub fn from_reporting_hosts(hosts: i64) -> Self {
        if hosts > 0 {
            StoragePoolState::Ready
        } else {
            StoragePoolState::Pending
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StoragePoolState::Pending => "pending",
            StoragePoolState::Ready => "ready",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_carry_their_kind_through_json() {
        let p = PoolParams::SharedDir {
            path: "/var/lib/vquasar/shared/volumes".into(),
        };
        let json = serde_json::to_value(&p).unwrap();
        // Internally tagged: the stored blob names its own shape, so a row
        // cannot be read as the wrong kind.
        assert_eq!(json["kind"], "shared_dir");
        assert_eq!(json["path"], "/var/lib/vquasar/shared/volumes");
        assert_eq!(serde_json::from_value::<PoolParams>(json).unwrap(), p);
        assert_eq!(p.kind(), StoragePoolKind::SharedDir);
        assert_eq!(p.kind().as_str(), "shared_dir");
    }

    #[test]
    fn kind_round_trips_through_strings() {
        assert_eq!(
            "shared_dir".parse::<StoragePoolKind>().unwrap(),
            StoragePoolKind::SharedDir
        );
        assert!("ceph".parse::<StoragePoolKind>().is_err());
    }

    #[test]
    fn a_shared_dir_needs_an_absolute_path() {
        use PoolValidationError as E;
        assert_eq!(
            PoolParams::SharedDir { path: "".into() }.validate(),
            Err(E::MissingPath)
        );
        assert_eq!(
            PoolParams::SharedDir { path: "  ".into() }.validate(),
            Err(E::MissingPath)
        );
        assert_eq!(
            PoolParams::SharedDir {
                path: "shared/volumes".into()
            }
            .validate(),
            Err(E::RelativePath("shared/volumes".into()))
        );
        assert!(PoolParams::SharedDir {
            path: "/srv/fast".into()
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn names_are_conservative() {
        use PoolValidationError as E;
        assert!(validate_pool_name("default").is_ok());
        assert!(validate_pool_name("fast-nvme-01").is_ok());
        assert_eq!(validate_pool_name(""), Err(E::EmptyName));
        assert_eq!(
            validate_pool_name("Fast"),
            Err(E::InvalidName("Fast".into()))
        );
        assert_eq!(
            validate_pool_name("-fast"),
            Err(E::InvalidName("-fast".into()))
        );
        assert_eq!(
            validate_pool_name("fast-"),
            Err(E::InvalidName("fast-".into()))
        );
        assert_eq!(
            validate_pool_name("fast/pool"),
            Err(E::InvalidName("fast/pool".into()))
        );
        assert_eq!(validate_pool_name(&"a".repeat(64)), Err(E::NameTooLong(64)));
    }

    /// A pool nobody reports is not usable, however correct it looks.
    #[test]
    fn state_follows_what_hosts_report() {
        assert_eq!(
            StoragePoolState::from_reporting_hosts(0),
            StoragePoolState::Pending
        );
        assert_eq!(
            StoragePoolState::from_reporting_hosts(1),
            StoragePoolState::Ready
        );
        assert_eq!(StoragePoolState::Ready.as_str(), "ready");
    }
}
