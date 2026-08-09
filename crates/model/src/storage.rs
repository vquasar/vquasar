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

/// Whether a pool's bytes are the same bytes on every host that reports it.
///
/// This is the distinction the rest of the storage model turns on, and getting
/// it wrong is silent. Every placement rule written before this existed assumed
/// "a host reports the pool" meant "that host can see this volume's data" —
/// true for shared storage, false for local, where two hosts reporting the same
/// pool have two different disks that merely share a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sharing {
    /// One filesystem, reachable from every host that reports it. A VM may be
    /// scheduled onto any of them and live-migrated between them.
    Shared,
    /// Storage attached to one host. A VM with a disk here is pinned to the
    /// host it was placed on: no other host can see those bytes, so a live
    /// migration is not slow — it is data loss.
    Local,
}

impl Sharing {
    pub fn is_shared(self) -> bool {
        self == Sharing::Shared
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Sharing::Shared => "shared",
            Sharing::Local => "local",
        }
    }

    /// A one-line statement for the API and the console.
    ///
    /// Part of the model rather than the UI: what a pool guarantees is a
    /// property of its kind, and an operator should not have to infer it from
    /// the kind's name.
    pub fn guarantee(self) -> &'static str {
        match self {
            Sharing::Shared => {
                "One filesystem seen by every host that reports it. VMs here can be live-migrated."
            }
            Sharing::Local => {
                "Storage on each host separately. A VM here is pinned to its host and cannot be live-migrated."
            }
        }
    }
}

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
    /// A directory on each host, holding that host's own bytes.
    ///
    /// Two hosts reporting this pool have two different disks that share a
    /// name — fast local NVMe, typically. A VM with a disk here is pinned to
    /// the host it landed on.
    LocalDir,
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
            StoragePoolKind::LocalDir => "local_dir",
            StoragePoolKind::Nfs => "nfs",
        }
    }

    /// Whether every host reporting this kind sees the *same* bytes.
    pub fn sharing(self) -> Sharing {
        match self {
            StoragePoolKind::SharedDir | StoragePoolKind::Nfs => Sharing::Shared,
            StoragePoolKind::LocalDir => Sharing::Local,
        }
    }
}

impl std::str::FromStr for StoragePoolKind {
    type Err = InvalidStoragePoolKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "shared_dir" => Ok(StoragePoolKind::SharedDir),
            "local_dir" => Ok(StoragePoolKind::LocalDir),
            "nfs" => Ok(StoragePoolKind::Nfs),
            other => Err(InvalidStoragePoolKind(other.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown storage pool kind {0:?} — expected shared_dir, local_dir or nfs")]
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
    /// The same shape as a shared directory, and deliberately a separate kind:
    /// nothing about the path says whether other hosts can see it, and that is
    /// exactly what placement needs to know.
    LocalDir {
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
            PoolParams::LocalDir { .. } => StoragePoolKind::LocalDir,
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
            PoolParams::SharedDir { .. } | PoolParams::LocalDir { .. } => None,
        }
    }

    /// Whether every host reporting this pool sees the same bytes.
    pub fn sharing(&self) -> Sharing {
        self.kind().sharing()
    }

    /// The pool's root as the agents see it, when the kind has one.
    ///
    /// A future networked kind (`rbd`, `nfs` before it is mounted) has no host
    /// path, which is why this is an `Option` rather than a field.
    pub fn host_path(&self) -> Option<&str> {
        match self {
            PoolParams::SharedDir { path } | PoolParams::LocalDir { path } => Some(path),
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
            PoolParams::SharedDir { path } | PoolParams::LocalDir { path } => absolute(path),
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
    /// The distinction the whole storage model turns on. Two hosts reporting a
    /// local pool have two different disks that share a name; two reporting a
    /// shared one have the same filesystem twice.
    #[test]
    fn a_kind_says_whether_its_bytes_are_shared() {
        assert!(StoragePoolKind::SharedDir.sharing().is_shared());
        assert!(StoragePoolKind::Nfs.sharing().is_shared());
        assert!(!StoragePoolKind::LocalDir.sharing().is_shared());
        assert_eq!(StoragePoolKind::LocalDir.sharing(), Sharing::Local);
        assert_eq!(StoragePoolKind::LocalDir.sharing().as_str(), "local");
        // The guarantee is stated, not left for a reader to infer from a name.
        assert!(Sharing::Local
            .guarantee()
            .contains("cannot be live-migrated"));
        assert!(Sharing::Shared.guarantee().contains("live-migrated"));
        assert_ne!(Sharing::Local.guarantee(), Sharing::Shared.guarantee());
    }

    /// A local directory looks exactly like a shared one — same field, same
    /// validation — and is a separate kind precisely because nothing about the
    /// path itself says whether other hosts can see it.
    #[test]
    fn a_local_directory_is_a_shared_one_that_admits_it_is_not() {
        let local = PoolParams::LocalDir {
            path: "/var/lib/vquasar/local".into(),
        };
        let shared = PoolParams::SharedDir {
            path: "/var/lib/vquasar/local".into(),
        };
        assert_eq!(local.host_path(), shared.host_path());
        assert!(local.validate().is_ok());
        assert_eq!(local.mount_source(), None);
        assert!(!local.sharing().is_shared() && shared.sharing().is_shared());
        let json = serde_json::to_value(&local).unwrap();
        assert_eq!(json["kind"], "local_dir");
        assert_eq!(serde_json::from_value::<PoolParams>(json).unwrap(), local);
        assert_eq!(
            "local_dir".parse::<StoragePoolKind>().unwrap(),
            StoragePoolKind::LocalDir
        );
    }

    #[test]
    fn a_local_directory_still_needs_an_absolute_path() {
        assert_eq!(
            PoolParams::LocalDir {
                path: "local".into()
            }
            .validate(),
            Err(PoolValidationError::RelativePath("local".into()))
        );
    }
}
