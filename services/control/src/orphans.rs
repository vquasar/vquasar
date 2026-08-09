//! Reclaiming files the platform created and no longer owns (#41, ADR-023).
//!
//! Deleting a VM removes its row; the cloud-init seed and the system disk it
//! was given live on shared storage, and until recently nothing removed those.
//! The agent cannot do this sweep — a file on shared storage may belong to a VM
//! on any host, so an agent deleting what it does not recognise would be
//! deleting another host's work. The control plane can, because it holds every
//! row, and now knows where "there" is: a pool.
//!
//! Three rules make a destructive sweep safe to run:
//!
//! * **Only files the platform provably made.** Ownership is decided by name,
//!   from the patterns this codebase writes and no others. An operator's file
//!   in a pool is never a candidate, whatever it contains — including the
//!   `.vquasar-probe-*` files the reachability check leaves behind.
//! * **Only when the row is gone.** Files are listed *first* and the ids read
//!   *after*: a resource created in between is then already in the id set and
//!   is kept. The other order would delete the disk of a VM created a
//!   millisecond too late.
//! * **Only when the file has settled.** A grace period on the modification
//!   time, so anything being written right now is left alone regardless.
//!
//! The default policy is [`Policy::Report`]: an operator learns what is leaking
//! before anything is deleted on their behalf. Reclaiming is opted into.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::store::Store;

/// What to do about files whose owner is gone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// Do not look.
    Off,
    /// Look and say what was found, but delete nothing. The default: an
    /// operator should learn a cluster is leaking before the platform starts
    /// removing files on their behalf.
    #[default]
    Report,
    /// Look and reclaim.
    Delete,
}

/// What a sweep found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sweep {
    pub found: usize,
    pub bytes: u64,
    pub reclaimed: usize,
}

/// The row a platform-created file belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Vm(Uuid),
    Volume(Uuid),
}

/// Which row this file belongs to, or `None` when the platform did not make it.
///
/// The patterns are exactly the ones written elsewhere in this codebase:
/// `vol-<uuid>.<ext>` for a volume, `<uuid>.<ext>` for a VM's system disk or
/// its cloud-init seed, and `<uuid>-disk<n>` / `<uuid>-d<n>` for the extra
/// disks a VM can be given. Anything else is somebody else's file.
fn owner(file_name: &str) -> Option<Owner> {
    let stem = file_name.rsplit_once('.').map(|(s, _)| s)?;
    if let Some(rest) = stem.strip_prefix("vol-") {
        return rest.parse().ok().map(Owner::Volume);
    }
    // A UUID is exactly 36 characters, so the id is a fixed-width prefix and
    // anything after it is the suffix. Searching for "-d" instead would cut a
    // UUID that happens to contain one — and plenty do.
    const UUID_LEN: usize = 36;
    let id: Uuid = stem.get(..UUID_LEN)?.parse().ok()?;
    match stem.get(UUID_LEN..)? {
        "" => Some(Owner::Vm(id)),
        suffix if is_disk_suffix(suffix) => Some(Owner::Vm(id)),
        _ => None,
    }
}

/// `-disk0` / `-d2`, the two shapes an extra VM disk is named with.
fn is_disk_suffix(s: &str) -> bool {
    s.strip_prefix("-disk")
        .or_else(|| s.strip_prefix("-d"))
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Every directory a sweep looks in: each `shared_dir` pool's root and the
/// `seeds` directory under it. Never recursive — a pool root may sit next to
/// directories this has no business in, and descending would eventually find
/// one.
async fn scan_dirs(store: &Store) -> Result<Vec<PathBuf>, sqlx::Error> {
    let mut dirs = Vec::new();
    for path in store.pool_paths().await?.into_values() {
        let root = PathBuf::from(path);
        dirs.push(root.join("seeds"));
        dirs.push(root);
    }
    Ok(dirs)
}

/// One candidate: a file the platform made, and who it was made for.
struct Candidate {
    path: PathBuf,
    owner: Owner,
    bytes: u64,
}

/// Files in `dir` that the platform made and that have stopped changing.
async fn candidates_in(dir: &Path, min_age: Duration) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        // A pool this control plane cannot read is not an error here: it may be
        // storage only the agents mount, and the reachability report already
        // says so far more precisely than a sweep could.
        Err(e) => {
            debug!(dir = %dir.display(), error = %e, "orphan sweep skipped a directory");
            return out;
        }
    };
    let now = SystemTime::now();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(owner) = owner(name) else { continue };
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        // Anything still being written is left alone whatever its name says.
        let settled = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= min_age);
        if !settled {
            continue;
        }
        out.push(Candidate {
            path: entry.path(),
            owner,
            bytes: meta.len(),
        });
    }
    out
}

/// Find, and possibly reclaim, files whose owning row is gone.
pub async fn sweep(store: &Store, policy: Policy, min_age: Duration) -> anyhow::Result<Sweep> {
    let mut result = Sweep::default();
    if policy == Policy::Off {
        return Ok(result);
    }

    // Listing happens before the id sets are read, and the order is
    // load-bearing: a VM created between the two is already in `vms` and its
    // disk is kept. Reversed, this would delete the disk of a VM that was
    // created a moment too late.
    let mut found = Vec::new();
    for dir in scan_dirs(store).await? {
        found.extend(candidates_in(&dir, min_age).await);
    }
    if found.is_empty() {
        return Ok(result);
    }
    let vms: HashSet<Uuid> = store.all_vm_ids().await?.into_iter().collect();
    let volumes: HashSet<Uuid> = store.all_volume_ids().await?.into_iter().collect();

    for c in found {
        let orphaned = match c.owner {
            Owner::Vm(id) => !vms.contains(&id),
            Owner::Volume(id) => !volumes.contains(&id),
        };
        if !orphaned {
            continue;
        }
        result.found += 1;
        result.bytes += c.bytes;
        if policy == Policy::Delete {
            match tokio::fs::remove_file(&c.path).await {
                Ok(()) => {
                    result.reclaimed += 1;
                    info!(file = %c.path.display(), bytes = c.bytes, "reclaimed an orphaned file");
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => result.reclaimed += 1,
                Err(e) => {
                    warn!(file = %c.path.display(), error = %e, "could not reclaim an orphaned file")
                }
            }
        } else {
            info!(file = %c.path.display(), bytes = c.bytes,
                  "orphaned file (set [storage] orphan_reclaim = \"delete\" to reclaim)");
        }
    }

    if result.found > 0 {
        let mib = result.bytes / (1024 * 1024);
        let message = if policy == Policy::Delete {
            format!(
                "reclaimed {} of {} orphaned file(s), {mib} MiB",
                result.reclaimed, result.found
            )
        } else {
            format!(
                "{} orphaned file(s) holding {mib} MiB; \
                 set [storage] orphan_reclaim = \"delete\" to reclaim them",
                result.found
            )
        };
        store
            .insert_event("storage", None, "storage.orphans", "info", &message)
            .await?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_file_names_are_attributed_to_their_row() {
        let id = Uuid::new_v4();
        assert_eq!(owner(&format!("vol-{id}.qcow2")), Some(Owner::Volume(id)));
        assert_eq!(owner(&format!("{id}.iso")), Some(Owner::Vm(id)));
        assert_eq!(owner(&format!("{id}.qcow2")), Some(Owner::Vm(id)));
        assert_eq!(owner(&format!("{id}-disk0.raw")), Some(Owner::Vm(id)));
        assert_eq!(owner(&format!("{id}-d2.qcow2")), Some(Owner::Vm(id)));
    }

    /// A UUID may contain "-d". Cutting the name at that marker instead of at
    /// the UUID's fixed width would fail to attribute the file — and an
    /// unattributed platform file leaks forever.
    #[test]
    fn a_uuid_containing_the_disk_marker_is_still_attributed() {
        let id: Uuid = "550e8400-d29b-41d4-a716-446655440000".parse().unwrap();
        assert!(id.to_string().contains("-d"));
        assert_eq!(owner(&format!("{id}.qcow2")), Some(Owner::Vm(id)));
        assert_eq!(owner(&format!("{id}-disk1.raw")), Some(Owner::Vm(id)));
        assert_eq!(owner(&format!("{id}.iso")), Some(Owner::Vm(id)));
    }

    /// A suffix the platform does not write is not the platform's file, even
    /// behind a real UUID: it is somebody's backup or scratch copy.
    #[test]
    fn a_uuid_with_an_unknown_suffix_is_left_alone() {
        let id = Uuid::new_v4();
        assert_eq!(owner(&format!("{id}-backup.qcow2")), None);
        // A hand-made copy: the stem is "<uuid>.qcow2", not "<uuid>".
        assert_eq!(owner(&format!("{id}.qcow2.bak")), None);
    }

    /// The rule that makes a destructive sweep safe: anything the platform did
    /// not name is not a candidate, whatever it holds.
    #[test]
    fn everything_else_belongs_to_somebody_else() {
        assert_eq!(owner("ubuntu-24.04.qcow2"), None);
        assert_eq!(owner("notes.txt"), None);
        // The reachability probe writes this into every pool it checks.
        assert_eq!(owner(".vquasar-probe-hostA"), None);
        // A name that merely looks like ours but is not a UUID.
        assert_eq!(owner("vol-not-a-uuid.qcow2"), None);
        assert_eq!(owner("12345678.qcow2"), None);
        // No extension at all: a directory, or something hand-made.
        assert_eq!(owner("seeds"), None);
    }

    #[tokio::test]
    async fn a_file_still_being_written_is_not_a_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        tokio::fs::write(dir.path().join(format!("{id}.qcow2")), b"x")
            .await
            .unwrap();
        // Freshly written, so a grace period of any real length excludes it.
        let fresh = candidates_in(dir.path(), Duration::from_secs(3600)).await;
        assert!(fresh.is_empty(), "a file written just now was a candidate");
        // With no grace period it is one.
        let settled = candidates_in(dir.path(), Duration::ZERO).await;
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].owner, Owner::Vm(id));
    }

    #[tokio::test]
    async fn a_directory_that_is_not_there_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("seeds");
        assert!(candidates_in(&missing, Duration::ZERO).await.is_empty());
    }

    #[tokio::test]
    async fn only_platform_files_are_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        for name in [
            format!("{id}.qcow2"),
            "ubuntu.qcow2".into(),
            ".vquasar-probe-hostA".into(),
        ] {
            tokio::fs::write(dir.path().join(name), b"x").await.unwrap();
        }
        let found = candidates_in(dir.path(), Duration::ZERO).await;
        assert_eq!(found.len(), 1, "swept a file the platform did not make");
        assert_eq!(found[0].owner, Owner::Vm(id));
    }
}
