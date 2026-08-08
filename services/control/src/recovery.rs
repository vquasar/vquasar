//! Reclaim work that was in flight when the process died (design §7).
//!
//! Two operations do expensive external work in a detached task after their row
//! is persisted: importing an image (download, then `qemu-img`) and provisioning
//! a volume (`qemu-img convert` on shared storage, ADR-019). Both leave the row
//! in a transitional state — `importing`, `provisioning` — that the detached
//! task is responsible for clearing.
//!
//! A restart kills that task. Nothing else ever clears the row, so before this
//! existed an image stayed `importing` forever and a volume stayed
//! `provisioning` forever — the latter still counting against its project's
//! quota, for a file that will never be finished.
//!
//! The sweep runs **once at startup and reclaims everything transitional**,
//! which is exact rather than a guess: this process has just started, so it owns
//! no detached tasks, and any row still in a transitional state was owned by an
//! instance that is gone. That is why there is no "stuck for longer than N
//! minutes" heuristic here — a legitimately slow 40 GB download and an orphan
//! are indistinguishable by age, and picking a timeout would eventually kill a
//! real import.
//!
//! **This exactness depends on there being one control plane.** With several
//! (ADR-021), a restarting instance would reclaim another instance's live work,
//! so the rows will need to record which instance owns them and the sweep will
//! have to filter on it. That belongs with the HA work, not ahead of it.
//!
//! A task that panics while the process keeps running still leaves its row
//! stuck until the next restart. Accepted: the alternative is the timeout this
//! deliberately avoids.

use tracing::{info, warn};

use crate::store::Store;

/// Fail every transitional row left behind by a previous process.
pub async fn reclaim_orphaned_work(store: &Store) {
    match store.fail_orphaned_imports().await {
        Ok(names) if !names.is_empty() => {
            info!(
                count = names.len(),
                images = ?names,
                "marked images failed: their import did not survive a restart"
            );
        }
        Ok(_) => {}
        Err(e) => warn!(error = %e, "could not reclaim orphaned image imports"),
    }

    match store.drop_orphaned_volume_reservations().await {
        Ok(dropped) if !dropped.is_empty() => {
            // Remove the half-written files too. Best-effort and after the
            // rows are gone: a leftover file wastes space, but a row that
            // outlives its file wastes quota and confuses every later create.
            for (id, format) in &dropped {
                let path =
                    crate::api::volumes::volume_path(store.shared_volumes_dir(), *id, format);
                if let Err(e) = tokio::fs::remove_file(&path).await {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        warn!(path = %path.display(), error = %e,
                              "could not remove a partial volume file");
                    }
                }
            }
            info!(
                count = dropped.len(),
                "dropped volume reservations whose provisioning did not survive a restart"
            );
        }
        Ok(_) => {}
        Err(e) => warn!(error = %e, "could not reclaim orphaned volume reservations"),
    }
}
