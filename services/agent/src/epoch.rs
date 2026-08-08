//! Refuse a superseded controller by lease epoch (ADR-022).
//!
//! ADR-021 bounds — but does not close — the window in which a control plane
//! that has lost its lease can still issue an agent RPC. The lease-margin check
//! happens in the caller, which is precisely the component that cannot be
//! trusted to be running; a process paused past its margin can wake and act.
//!
//! So the agent keeps the highest epoch it has ever been told and refuses
//! anything lower. That is deliberately all it does. It does not read the lease,
//! does not evaluate who *should* be leader, and holds no opinion about the
//! control plane — comparing one integer against the largest it has seen is
//! bookkeeping, not judgement, which is what keeps this on the right side of
//! ADR-001 and §30.
//!
//! **What this does not fix.** Fencing addresses a controller that is
//! superseded *but alive*. When a leader dies mid-operation, its successor
//! carries a **higher** epoch and is admitted by design — so the duplicate
//! `PrepareReceive` that motivated this ADR is not prevented by it. That case
//! needs at-most-once semantics on the agent (issue #45); the two are
//! complementary, and neither is a substitute for the other.
//!
//! **Persistence.** The number outlives the process. An agent that forgot it on
//! restart would reopen exactly the window this closes: a stale controller would
//! be believed by an agent that had just come back. It is stored under the
//! agent's state directory rather than `runtime_dir`, because `/run` is tmpfs
//! and a reboot is a restart.
//!
//! **An absent epoch is accepted, and logged** — until strict mode is on. That
//! is the migration path: agents are upgraded first and must tolerate a
//! controller that does not stamp yet, or a rolling upgrade breaks a deployed
//! cluster (ADR-005). The warning is what tells an operator the fleet is not
//! ready to enforce.

// tonic's `Status` is a large error type used pervasively by the generated
// trait; boxing every return would fight the API for no benefit.
#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use tonic::metadata::MetadataMap;
use tonic::Status;

/// The metadata key the controller stamps. ASCII, lowercase — gRPC requires it.
pub const EPOCH_KEY: &str = "x-vquasar-controller-epoch";

/// Highest controller epoch this agent has seen, and whether to insist on one.
pub struct EpochGuard {
    /// Where the number is persisted. `None` disables persistence (tests).
    path: Option<PathBuf>,
    highest: AtomicI64,
    /// Refuse a request that carries no epoch at all. Off until a fleet is
    /// known to be fully upgraded — see the module note on the migration path.
    strict: bool,
    /// So the "controller is not stamping" warning is one line per process
    /// rather than one per RPC. A reconcile tick talks to every host every
    /// second; at that rate the warning would bury the log it is meant to be
    /// read from.
    warned_absent: AtomicBool,
}

impl EpochGuard {
    /// Load the persisted epoch from `path`, or start at zero.
    ///
    /// An unreadable or corrupt file starts at zero and warns rather than
    /// failing to start. Refusing to boot would take a host's VMs offline over
    /// a fencing counter, which is a worse outcome than the window it reopens —
    /// and the window closes again on the first stamped request.
    pub fn load(path: impl Into<PathBuf>, strict: bool) -> Self {
        let path = path.into();
        let highest = match std::fs::read_to_string(&path) {
            Ok(s) => match s.trim().parse::<i64>() {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e,
                        "controller epoch file is not a number; starting from zero");
                    0
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e,
                    "cannot read the controller epoch; starting from zero");
                0
            }
        };
        if highest > 0 {
            tracing::info!(
                epoch = highest,
                "resuming from the persisted controller epoch"
            );
        }
        Self {
            path: Some(path),
            highest: AtomicI64::new(highest),
            strict,
            warned_absent: AtomicBool::new(false),
        }
    }

    /// A guard that forgets on drop. Tests only — the running agent always
    /// persists, because forgetting is the failure mode this guards against.
    #[cfg(test)]
    pub fn in_memory(strict: bool) -> Self {
        Self {
            path: None,
            highest: AtomicI64::new(0),
            strict,
            warned_absent: AtomicBool::new(false),
        }
    }

    pub fn highest(&self) -> i64 {
        self.highest.load(Ordering::Acquire)
    }

    /// Admit or refuse one request.
    ///
    /// Per-RPC, not per-connection: a connection outlives a lease, and tearing
    /// one down over a stale call would take working requests with it.
    pub fn check(&self, md: &MetadataMap) -> Result<(), Status> {
        let Some(raw) = md.get(EPOCH_KEY) else {
            return self.absent();
        };
        let epoch = raw
            .to_str()
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .ok_or_else(|| {
                // Malformed is not the same as absent. Absence is a controller
                // that predates this; a value we cannot parse is a bug or a
                // forgery, and waving it through would make the header
                // trivially bypassable by sending garbage.
                tracing::warn!("rejected a controller RPC: unparsable epoch");
                Status::permission_denied("unparsable controller epoch")
            })?;

        // `fetch_max` decides and records in one step, so two concurrent RPCs
        // cannot both read the old value and race to write it back.
        let previous = self.highest.fetch_max(epoch, Ordering::AcqRel);
        if epoch < previous {
            tracing::warn!(
                epoch,
                highest = previous,
                "rejected a superseded controller: its lease epoch is behind one already seen"
            );
            return Err(Status::permission_denied(format!(
                "controller epoch {epoch} is behind the highest seen ({previous})"
            )));
        }
        if epoch > previous {
            tracing::info!(epoch, previous, "controller epoch advanced");
            self.persist(epoch);
        }
        Ok(())
    }

    fn absent(&self) -> Result<(), Status> {
        if self.strict {
            tracing::warn!(
                "rejected a controller RPC carrying no lease epoch ([grpc] require_controller_epoch is on)"
            );
            return Err(Status::permission_denied(
                "controller epoch required but absent",
            ));
        }
        if !self.warned_absent.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "the control plane is not stamping a lease epoch; a superseded controller \
                 cannot be refused. Upgrade the control plane, then set \
                 [grpc] require_controller_epoch = true (ADR-022)"
            );
        }
        Ok(())
    }

    /// Write the epoch out. Best-effort and deliberately not fatal: the request
    /// has already been admitted on a value held in memory, and failing the RPC
    /// now would refuse a legitimate controller over a disk problem. What a
    /// failed write costs is the guarantee across a restart, which is what the
    /// error says.
    fn persist(&self, epoch: i64) {
        let Some(path) = &self.path else { return };
        if let Err(e) = write_atomically(path, epoch) {
            tracing::error!(path = %path.display(), error = %e, epoch,
                "could not persist the controller epoch; a restart would forget it");
        }
    }
}

/// Write-and-rename, so a crash mid-write cannot leave a truncated file that
/// reads back as a *lower* epoch than the agent had already accepted.
fn write_atomically(path: &Path, epoch: i64) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{epoch}\n"))?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(epoch: &str) -> MetadataMap {
        let mut m = MetadataMap::new();
        m.insert(EPOCH_KEY, epoch.parse().unwrap());
        m
    }

    #[test]
    fn the_first_epoch_seen_is_accepted_and_remembered() {
        let g = EpochGuard::in_memory(false);
        assert!(g.check(&md("7")).is_ok());
        assert_eq!(g.highest(), 7);
    }

    #[test]
    fn a_superseded_controller_is_refused() {
        let g = EpochGuard::in_memory(false);
        g.check(&md("7")).unwrap();
        let err = g.check(&md("6")).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        // The refusal must not lower the bar for the next caller.
        assert_eq!(g.highest(), 7);
    }

    /// The same epoch arrives on every RPC of a term; only a *lower* one is a
    /// superseded controller.
    #[test]
    fn the_incumbent_keeps_working() {
        let g = EpochGuard::in_memory(false);
        for _ in 0..5 {
            assert!(g.check(&md("7")).is_ok());
        }
        assert_eq!(g.highest(), 7);
    }

    /// The successor's epoch is higher, so failover is admitted — this is the
    /// case ADR-022 must *not* break.
    #[test]
    fn a_successor_takes_over() {
        let g = EpochGuard::in_memory(false);
        g.check(&md("7")).unwrap();
        assert!(g.check(&md("8")).is_ok());
        assert_eq!(g.highest(), 8);
    }

    /// The migration path: an agent upgraded ahead of its control plane must
    /// keep working.
    #[test]
    fn an_absent_epoch_is_accepted_until_strict_mode() {
        let lenient = EpochGuard::in_memory(false);
        assert!(lenient.check(&MetadataMap::new()).is_ok());

        let strict = EpochGuard::in_memory(true);
        let err = strict.check(&MetadataMap::new()).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    /// Garbage is refused in both modes: accepting it would make the check
    /// bypassable by sending something unparsable instead of nothing.
    #[test]
    fn an_unparsable_epoch_is_refused_even_when_lenient() {
        let g = EpochGuard::in_memory(false);
        assert_eq!(
            g.check(&md("not-a-number")).unwrap_err().code(),
            tonic::Code::PermissionDenied
        );
    }

    /// The whole point of persisting: an agent that forgot the epoch would
    /// believe a stale controller after a restart.
    #[test]
    fn the_epoch_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("controller-epoch");

        let before = EpochGuard::load(&path, false);
        before.check(&md("9")).unwrap();

        let after = EpochGuard::load(&path, false);
        assert_eq!(after.highest(), 9, "the epoch did not survive a restart");
        assert_eq!(
            after.check(&md("8")).unwrap_err().code(),
            tonic::Code::PermissionDenied,
            "a restarted agent believed a superseded controller"
        );
    }

    #[test]
    fn a_corrupt_epoch_file_does_not_stop_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("controller-epoch");
        std::fs::write(&path, "nonsense").unwrap();
        let g = EpochGuard::load(&path, false);
        assert_eq!(g.highest(), 0);
        assert!(g.check(&md("3")).is_ok());
    }
}
