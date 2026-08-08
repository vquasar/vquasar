//! Probing storage pools: what this host can actually do with them
//! (design §20, ADR-023).
//!
//! The control plane owns the list of pools; this module owns the only
//! question the control plane cannot answer for itself — whether *this* host
//! can use one, and how much room it has. That is why reachability is observed
//! rather than declared: an operator recording "every host has this mounted"
//! records an intention, and the filesystem is free to disagree.
//!
//! Two details are load-bearing:
//!
//! * **The directory is never created.** A host missing the mount would
//!   otherwise get a fresh empty directory on its own root filesystem, and
//!   would then report the pool as usable — writing what it believed was
//!   shared storage somewhere no other host can see. That is the exact failure
//!   this reports on, so `mkdir` here would defeat the whole thing.
//! * **Writability is proved, not inferred.** A `shared_dir` pool that exists
//!   and is readable can still be unwritable — a read-only remount, or NFS
//!   `root_squash`, which permission bits alone say nothing about. So the
//!   probe writes a file.

use std::path::Path;

use vquasar_proto::agent::{StoragePoolProbe, StoragePoolReport};

/// Check every pool the control plane asked about.
///
/// `host_id` names the probe file, so two hosts checking the same shared
/// directory cannot collide on it.
pub async fn probe_all(probes: &[StoragePoolProbe], host_id: &str) -> Vec<StoragePoolReport> {
    let mut out = Vec::with_capacity(probes.len());
    for p in probes {
        out.push(probe_one(p, host_id).await);
    }
    out
}

async fn probe_one(probe: &StoragePoolProbe, host_id: &str) -> StoragePoolReport {
    let unusable = |message: String| StoragePoolReport {
        pool_id: probe.pool_id.clone(),
        usable: false,
        message,
        capacity_bytes: 0,
        available_bytes: 0,
    };
    match probe.kind.as_str() {
        "shared_dir" => match shared_dir(Path::new(&probe.path), host_id).await {
            Ok((capacity_bytes, available_bytes)) => StoragePoolReport {
                pool_id: probe.pool_id.clone(),
                usable: true,
                message: String::new(),
                capacity_bytes,
                available_bytes,
            },
            Err(why) => unusable(why),
        },
        // A kind this agent does not implement is reported as unusable rather
        // than skipped: "the host is older than the pool" is a real answer, and
        // silence would read as the pool simply not existing here.
        other => unusable(format!("this agent does not support the {other} pool kind")),
    }
}

/// Capacity and free space for a `shared_dir` pool, or why it is not usable.
async fn shared_dir(path: &Path, host_id: &str) -> Result<(u64, u64), String> {
    match tokio::fs::metadata(path).await {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(format!("{} is not a directory", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{} does not exist here — the pool is probably not mounted on this host",
                path.display()
            ))
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }

    let probe = path.join(format!(".vquasar-probe-{host_id}"));
    if let Err(e) = tokio::fs::write(&probe, b"").await {
        return Err(format!("not writable: {e}"));
    }
    // Best-effort: a probe file left behind by a crash is harmless and the next
    // tick overwrites it, so a failure to clean up is not a reason to call the
    // pool unusable.
    let _ = tokio::fs::remove_file(&probe).await;

    let path = path.to_path_buf();
    let stat = tokio::task::spawn_blocking(move || rustix::fs::statvfs(&path))
        .await
        .map_err(|e| format!("statvfs task: {e}"))?
        .map_err(|e| format!("statvfs: {e}"))?;
    // f_frsize is the fragment size the block counts are in; f_bavail excludes
    // the root-reserved blocks, so it is what an unprivileged writer really has.
    let unit = stat.f_frsize;
    Ok((
        stat.f_blocks.saturating_mul(unit),
        stat.f_bavail.saturating_mul(unit),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(kind: &str, path: &str) -> StoragePoolProbe {
        StoragePoolProbe {
            pool_id: "p1".into(),
            name: "pool".into(),
            kind: kind.into(),
            path: path.into(),
        }
    }

    #[tokio::test]
    async fn a_real_directory_is_usable_and_reports_room() {
        let dir = tempfile::tempdir().unwrap();
        let r = probe_one(&probe("shared_dir", dir.path().to_str().unwrap()), "h1").await;
        assert!(r.usable, "{}", r.message);
        assert!(r.capacity_bytes > 0, "no capacity reported");
        assert!(r.available_bytes <= r.capacity_bytes);
    }

    /// The failure the whole resource exists for: a host without the mount.
    #[tokio::test]
    async fn a_missing_directory_is_unusable_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-mounted");
        let r = probe_one(&probe("shared_dir", missing.to_str().unwrap()), "h1").await;
        assert!(!r.usable);
        assert!(r.message.contains("not mounted"), "{}", r.message);
        // And it must not have been conjured into existence on the way past: a
        // host that creates the mount point writes "shared" data nobody else
        // can see.
        assert!(!missing.exists(), "the probe created the pool directory");
    }

    #[tokio::test]
    async fn a_file_where_a_directory_should_be_is_unusable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("regular");
        tokio::fs::write(&file, b"x").await.unwrap();
        let r = probe_one(&probe("shared_dir", file.to_str().unwrap()), "h1").await;
        assert!(!r.usable);
        assert!(r.message.contains("not a directory"), "{}", r.message);
    }

    /// A pool kind this agent is too old to understand is reported as such,
    /// not passed over in silence.
    #[tokio::test]
    async fn an_unknown_kind_is_refused_with_a_reason() {
        let r = probe_one(&probe("rbd", ""), "h1").await;
        assert!(!r.usable);
        assert!(r.message.contains("does not support"), "{}", r.message);
    }

    #[tokio::test]
    async fn the_probe_file_does_not_outlive_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let r = probe_one(&probe("shared_dir", dir.path().to_str().unwrap()), "h1").await;
        assert!(r.usable);
        assert!(!dir.path().join(".vquasar-probe-h1").exists());
    }

    #[tokio::test]
    async fn every_probe_gets_exactly_one_report() {
        let dir = tempfile::tempdir().unwrap();
        let reports = probe_all(
            &[
                probe("shared_dir", dir.path().to_str().unwrap()),
                probe("rbd", ""),
            ],
            "h1",
        )
        .await;
        assert_eq!(reports.len(), 2);
        assert!(reports[0].usable && !reports[1].usable);
    }
}
