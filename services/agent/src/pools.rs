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
//! * **A `shared_dir` is never created.** A host missing the mount would
//!   otherwise get a fresh empty directory on its own root filesystem, and
//!   would then report the pool as usable — writing what it believed was
//!   shared storage somewhere no other host can see. That is the exact failure
//!   this reports on, so `mkdir` here would defeat the whole thing.
//! * **Writability is proved, not inferred.** A pool that exists and is
//!   readable can still be unwritable — a read-only remount, or NFS
//!   `root_squash`, which permission bits alone say nothing about. So the
//!   probe writes a file.
//!
//! An `nfs` pool is the one kind this agent *mounts*. The mount point may be
//! created here, and the rule above is not weakened by it: an `nfs` pool is
//! only usable once `/proc/mounts` shows the export mounted there, so an empty
//! directory left by a failed mount fails the check rather than passing it.
//! Nothing is ever unmounted — a guest may have a disk open on it, and tidying
//! up a mount is not worth taking a VM's storage away.

use std::path::Path;

use tracing::info;
use vquasar_model::PoolParams;
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
    // Prefer the full parameters; fall back to the bare path so a control plane
    // that predates `params` still drives a shared directory.
    let params = match (probe.params.is_empty(), probe.kind.as_str()) {
        // The fallback is for a *shared directory* specifically. Reading an
        // unknown kind as one would report someone else's pool as this host's
        // directory, which is a worse answer than "I do not know that kind".
        (true, "shared_dir") => Ok(PoolParams::SharedDir {
            path: probe.path.clone(),
        }),
        (true, "local_dir") => Ok(PoolParams::LocalDir {
            path: probe.path.clone(),
        }),
        (true, _) => Err(()),
        (false, _) => serde_json::from_str::<PoolParams>(&probe.params).map_err(|_| ()),
    };
    let params = match params {
        Ok(p) => p,
        // A kind this agent does not know deserialises to nothing. Reported as
        // unusable rather than skipped: "the host is older than the pool" is a
        // real answer, and silence would read as the pool not existing here.
        Err(_) => {
            return unusable(format!(
                "this agent does not support the {} pool kind",
                probe.kind
            ))
        }
    };

    let ready = match &params {
        // A local directory is probed exactly like a shared one, and is not
        // created either: the usual reason it is missing is an NVMe that did
        // not mount, and quietly using the root filesystem instead is the same
        // failure with a different blast radius.
        PoolParams::SharedDir { .. } | PoolParams::LocalDir { .. } => Ok(()),
        PoolParams::Nfs { .. } => ensure_mounted(&params).await,
    };
    if let Err(why) = ready {
        return unusable(why);
    }
    let Some(path) = params.host_path() else {
        return unusable(format!(
            "the {} pool kind has no host path on this agent",
            probe.kind
        ));
    };
    match shared_dir(Path::new(path), host_id).await {
        Ok((capacity_bytes, available_bytes)) => StoragePoolReport {
            pool_id: probe.pool_id.clone(),
            usable: true,
            message: String::new(),
            capacity_bytes,
            available_bytes,
        },
        Err(why) => unusable(why),
    }
}

/// Make sure an `nfs` pool's export is mounted where the pool says.
///
/// Idempotent by design — this runs on every reconcile tick — so the common
/// path is a read of `/proc/mounts` and nothing else.
async fn ensure_mounted(params: &PoolParams) -> Result<(), String> {
    let (Some(target), Some(source)) = (params.host_path(), params.mount_source()) else {
        return Ok(());
    };
    if is_mounted(target, &source).await? {
        return Ok(());
    }
    // Creating the mount point is safe *here* precisely because the check above
    // is not "does the directory exist": a failed mount leaves a bare directory
    // that still reports the pool unusable.
    tokio::fs::create_dir_all(target)
        .await
        .map_err(|e| format!("could not create the mount point {target}: {e}"))?;

    let mut cmd = tokio::process::Command::new("mount");
    cmd.arg("-t").arg("nfs");
    if let PoolParams::Nfs {
        options: Some(o), ..
    } = params
    {
        cmd.arg("-o").arg(o);
    }
    cmd.arg(&source).arg(target);
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("could not run mount: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("mounting {source} on {target} failed: {why}"));
    }
    // Re-check rather than trusting the exit status: what makes the pool usable
    // is the mount being there, and that is what the next tick will ask too.
    if !is_mounted(target, &source).await? {
        return Err(format!(
            "mount reported success but {target} is still not {source}"
        ));
    }
    info!(%source, %target, "mounted an nfs storage pool");
    Ok(())
}

/// Whether `target` is a mount of `source`, from `/proc/mounts`.
async fn is_mounted(target: &str, source: &str) -> Result<bool, String> {
    let mounts = tokio::fs::read_to_string("/proc/mounts")
        .await
        .map_err(|e| format!("could not read /proc/mounts: {e}"))?;
    Ok(mount_present(&mounts, target, source))
}

/// The `/proc/mounts` half, split out so it can be tested without a mount.
///
/// The source is compared as well as the target: a directory that is a mount
/// of something *else* is not this pool, and treating it as one would put a
/// pool's volumes on a stranger's filesystem.
fn mount_present(mounts: &str, target: &str, source: &str) -> bool {
    mounts.lines().any(|line| {
        let mut f = line.split_whitespace();
        let (Some(dev), Some(mnt), Some(fstype)) = (f.next(), f.next(), f.next()) else {
            return false;
        };
        // /proc/mounts octal-escapes spaces and friends; a pool path with one
        // is refused long before it gets here.
        mnt == target && dev == source && fstype.starts_with("nfs")
    })
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
            params: String::new(),
        }
    }

    fn with_params(kind: &str, params: &PoolParams) -> StoragePoolProbe {
        StoragePoolProbe {
            pool_id: "p1".into(),
            name: "pool".into(),
            kind: kind.into(),
            path: params.host_path().unwrap_or_default().into(),
            params: serde_json::to_string(params).unwrap(),
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

    /// A `shared_dir` sent as full parameters behaves exactly as the bare path
    /// does — the fallback and the real thing must not diverge.
    #[tokio::test]
    async fn params_and_the_legacy_path_agree_for_a_shared_dir() {
        let dir = tempfile::tempdir().unwrap();
        let p = PoolParams::SharedDir {
            path: dir.path().to_string_lossy().into_owned(),
        };
        let a = probe_one(&with_params("shared_dir", &p), "h1").await;
        let b = probe_one(&probe("shared_dir", dir.path().to_str().unwrap()), "h1").await;
        assert!(a.usable && b.usable);
        assert_eq!(a.capacity_bytes, b.capacity_bytes);
    }

    /// An NFS pool whose export is not mounted is unusable — and the mount
    /// point being there is not what makes it usable.
    #[tokio::test]
    async fn an_nfs_pool_is_unusable_until_its_export_is_mounted() {
        let dir = tempfile::tempdir().unwrap();
        let mount_point = dir.path().join("nfs");
        // The directory exists and is writable; only the mount is missing.
        tokio::fs::create_dir_all(&mount_point).await.unwrap();
        let p = PoolParams::Nfs {
            server: "127.0.0.1".into(),
            export: "/exports/none".into(),
            mount_point: mount_point.to_string_lossy().into_owned(),
            options: None,
        };
        let r = probe_one(&with_params("nfs", &p), "h1").await;
        assert!(
            !r.usable,
            "an unmounted export passed because its directory existed: {}",
            r.message
        );
        assert!(
            r.message.contains("mount") || r.message.contains("/proc/mounts"),
            "{}",
            r.message
        );
    }

    /// The `/proc/mounts` check compares the source too. A directory that is a
    /// mount of something else is not this pool, and accepting it would put the
    /// pool's volumes on a stranger's filesystem.
    #[test]
    fn a_mount_of_something_else_is_not_this_pool() {
        let mounts = "10.0.0.5:/exports/vms /mnt/vms nfs4 rw,relatime 0 0
10.0.0.9:/other /mnt/vms nfs4 rw 0 0
tmpfs /mnt/scratch tmpfs rw 0 0
";
        assert!(mount_present(mounts, "/mnt/vms", "10.0.0.5:/exports/vms"));
        // Right path, wrong export.
        assert!(!mount_present(mounts, "/mnt/vms", "10.0.0.5:/elsewhere"));
        // Right export, wrong path.
        assert!(!mount_present(
            mounts,
            "/mnt/other",
            "10.0.0.5:/exports/vms"
        ));
        // Not NFS at all.
        assert!(!mount_present(mounts, "/mnt/scratch", "tmpfs"));
        assert!(!mount_present("", "/mnt/vms", "10.0.0.5:/exports/vms"));
    }

    /// A kind this agent is too old to understand: reported, not skipped.
    #[tokio::test]
    async fn an_unknown_kind_in_params_is_refused_with_a_reason() {
        let mut p = probe("rbd", "");
        p.params = r#"{"kind":"rbd","pool":"vms"}"#.into();
        let r = probe_one(&p, "h1").await;
        assert!(!r.usable);
        assert!(r.message.contains("does not support"), "{}", r.message);
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
