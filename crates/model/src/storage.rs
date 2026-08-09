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
    /// The platform does not mount it: an operator does, by whatever means
    /// their fleet already uses, and the agents report who managed to.
    #[default]
    SharedDir,
    /// An NFS export the *agents* mount, at a path the pool names.
    ///
    /// The difference from a `shared_dir` that happens to be NFS is who is
    /// responsible for the mount. Here the pool records the export, so a host
    /// that is missing it gets it — rather than an operator having to arrange
    /// the same mount on every host and nothing recording that they did.
    Nfs,
}

impl StoragePoolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StoragePoolKind::SharedDir => "shared_dir",
            StoragePoolKind::Nfs => "nfs",
        }
    }
}

impl std::str::FromStr for StoragePoolKind {
    type Err = InvalidStoragePoolKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "shared_dir" => Ok(StoragePoolKind::SharedDir),
            "nfs" => Ok(StoragePoolKind::Nfs),
            other => Err(InvalidStoragePoolKind(other.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown storage pool kind {0:?} — expected shared_dir or nfs")]
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
    SharedDir {
        path: String,
    },
    Nfs {
        /// Server address, as the hosts reach it.
        server: String,
        /// Export path on the server.
        export: String,
        /// Where the agents mount it. Volumes live under this, so it is the
        /// pool's host path in exactly the way a `shared_dir` path is.
        mount_point: String,
        /// Extra `mount -o` options. `None` leaves the client defaults alone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<String>,
    },
}

impl PoolParams {
    pub fn kind(&self) -> StoragePoolKind {
        match self {
            PoolParams::SharedDir { .. } => StoragePoolKind::SharedDir,
            PoolParams::Nfs { .. } => StoragePoolKind::Nfs,
        }
    }

    /// `server:/export`, the thing `mount` names and `/proc/mounts` reports —
    /// which is how a host tells "mounted" from "an empty directory with the
    /// right name".
    pub fn mount_source(&self) -> Option<String> {
        match self {
            PoolParams::Nfs { server, export, .. } => {
                Some(format!("{}:{}", server.trim(), export.trim()))
            }
            PoolParams::SharedDir { .. } => None,
        }
    }

    /// The pool's root as the agents see it, when the kind has one.
    ///
    /// A future networked kind (`rbd`, `nfs` before it is mounted) has no host
    /// path, which is why this is an `Option` rather than a field.
    pub fn host_path(&self) -> Option<&str> {
        match self {
            PoolParams::SharedDir { path } => Some(path),
            PoolParams::Nfs { mount_point, .. } => Some(mount_point),
        }
    }

    /// Check the parameters are coherent on their own terms.
    ///
    /// Confinement to the platform's storage roots is *not* checked here: the
    /// permitted roots are control-plane configuration, so that check lives
    /// with the API alongside the one for VM disk paths (design §30).
    pub fn validate(&self) -> Result<(), PoolValidationError> {
        let absolute = |p: &str| -> Result<(), PoolValidationError> {
            if p.trim().is_empty() {
                return Err(PoolValidationError::MissingPath);
            }
            if !p.starts_with('/') {
                return Err(PoolValidationError::RelativePath(p.to_string()));
            }
            Ok(())
        };
        match self {
            PoolParams::SharedDir { path } => absolute(path),
            PoolParams::Nfs {
                server,
                export,
                mount_point,
                options,
            } => {
                if server.trim().is_empty() {
                    return Err(PoolValidationError::MissingServer);
                }
                // A server field carrying mount syntax is an operator writing
                // `10.0.0.5:/exports` into the wrong box; the two halves are
                // separate here so the mount source can be built exactly once.
                if server.contains(':') || server.contains('/') {
                    return Err(PoolValidationError::ServerNotAnAddress(server.clone()));
                }
                absolute(export)?;
                absolute(mount_point)?;
                // Options go on a `mount -o` command line. Nothing here should
                // ever contain whitespace or a shell metacharacter, and a value
                // that does is either a mistake or an attempt at one.
                if let Some(o) = options {
                    if o.chars()
                        .any(|c| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '`' | '$'))
                    {
                        return Err(PoolValidationError::BadMountOptions(o.clone()));
                    }
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
    #[error("an nfs pool requires a server")]
    MissingServer,
    #[error(
        "server must be a host or address on its own, not {0:?} — the export is a separate field"
    )]
    ServerNotAnAddress(String),
    #[error("mount options must be a comma-separated list with no whitespace or shell characters, got {0:?}")]
    BadMountOptions(String),
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
    fn an_nfs_pool_carries_its_export_and_where_it_is_mounted() {
        let p = PoolParams::Nfs {
            server: "10.0.0.5".into(),
            export: "/exports/vms".into(),
            mount_point: "/var/lib/vquasar/nfs/fast".into(),
            options: Some("vers=4.2,hard".into()),
        };
        assert!(p.validate().is_ok());
        assert_eq!(p.kind(), StoragePoolKind::Nfs);
        // The mount point is the pool's host path: volumes live under it, and
        // everything downstream treats it exactly like a shared directory.
        assert_eq!(p.host_path(), Some("/var/lib/vquasar/nfs/fast"));
        assert_eq!(p.mount_source().as_deref(), Some("10.0.0.5:/exports/vms"));
        // A shared_dir has nothing to mount, which is the whole difference.
        assert_eq!(
            PoolParams::SharedDir { path: "/x".into() }.mount_source(),
            None
        );
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["kind"], "nfs");
        assert_eq!(serde_json::from_value::<PoolParams>(json).unwrap(), p);
    }

    #[test]
    fn an_nfs_pool_is_checked_before_it_reaches_a_mount_command() {
        use PoolValidationError as E;
        let nfs = |server: &str, export: &str, mount: &str, opts: Option<&str>| PoolParams::Nfs {
            server: server.into(),
            export: export.into(),
            mount_point: mount.into(),
            options: opts.map(str::to_string),
        };
        assert_eq!(nfs("", "/e", "/m", None).validate(), Err(E::MissingServer));
        // The classic mistake: mount syntax typed into the server box, which
        // would produce `10.0.0.5:/exports:/exports`.
        assert_eq!(
            nfs("10.0.0.5:/exports", "/e", "/m", None).validate(),
            Err(E::ServerNotAnAddress("10.0.0.5:/exports".into()))
        );
        assert_eq!(
            nfs("s", "exports", "/m", None).validate(),
            Err(E::RelativePath("exports".into()))
        );
        assert_eq!(
            nfs("s", "/e", "m", None).validate(),
            Err(E::RelativePath("m".into()))
        );
        // Options end up on a command line; a value with a shell character in
        // it is a mistake at best.
        assert_eq!(
            nfs("s", "/e", "/m", Some("hard; rm -rf /")).validate(),
            Err(E::BadMountOptions("hard; rm -rf /".into()))
        );
        assert!(nfs("s", "/e", "/m", Some("vers=4.2,hard,noatime"))
            .validate()
            .is_ok());
    }

    #[test]
    fn kind_round_trips_through_strings() {
        assert_eq!(
            "shared_dir".parse::<StoragePoolKind>().unwrap(),
            StoragePoolKind::SharedDir
        );
        assert_eq!(
            "nfs".parse::<StoragePoolKind>().unwrap(),
            StoragePoolKind::Nfs
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
