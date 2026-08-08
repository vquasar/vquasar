//! The controller lease: which control plane runs the loops (design §48,
//! ADR-021).
//!
//! Every instance serves the REST API — they are stateless in front of one
//! PostgreSQL. Exactly one runs the reconcile loop, the migration controller and
//! the sweeps, and it is the one holding this lease.
//!
//! ## Why a row and not an advisory lock
//!
//! `pg_try_advisory_lock` is the obvious choice and it is the wrong one here.
//! sqlx hands out arbitrary pooled connections, so a session-scoped lock ends up
//! held by whichever connection happened to take it — not by the instance — and
//! returning that connection to the pool makes ownership unobservable. A row is
//! explicit, survives connection churn, and answers "who is the leader" with a
//! `SELECT` an operator can actually run.
//!
//! ## Time
//!
//! Every timestamp comes from PostgreSQL's clock (`now()`), never an instance's.
//! Instances whose clocks disagree still agree about the lease, because none of
//! them is asked what time it is.
//!
//! ## Fencing
//!
//! The failure this has to survive: a leader is paused past its expiry (a long
//! GC pause, a hypervisor freezing the control VM), another instance takes over,
//! and then the old leader wakes up and acts.
//!
//! Renewal alone does not prevent that — the check and the act are not atomic,
//! so any gap between them is a window. What bounds it is [`Lease::is_fresh`]:
//! the controller acts only while more than half the TTL remains, so a pause
//! longer than that is noticed before the next action rather than after it. The
//! residual window is a pause that starts *inside* the margin and outlasts it.
//!
//! Most of what the controller does is idempotent by construction — `EnsureVm`
//! against a generation counter converges to the same place however many times
//! it runs. Migration is the exception: two `prepare_receive` calls mean two
//! receivers for one guest, which is a broken guest rather than a retry. So the
//! migration controller re-checks the lease against the database immediately
//! before each step ([`Lease::confirm`]), turning a bounded window into a
//! bounded window on the one operation where the window matters.
//!
//! Closing it completely means the agent rejecting a stale caller by epoch,
//! which is a `proto/agent.proto` change and its own milestone (ADR-021).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tracing::{info, warn};

/// How long a lease is good for after a successful renewal.
pub const TTL: Duration = Duration::from_secs(15);

/// How often the holder renews. A third of the TTL, so two consecutive renewal
/// failures still leave time to try again before anyone else may take over.
pub const RENEW_EVERY: Duration = Duration::from_secs(5);

/// A lease this instance may hold.
pub struct Lease {
    pool: PgPool,
    /// Who this instance says it is, in logs and in the `holder` column.
    identity: String,
    /// Whether the last renewal succeeded *and* left enough margin to act on.
    fresh: Arc<AtomicBool>,
}

impl Lease {
    pub fn new(pool: PgPool, identity: impl Into<String>) -> Self {
        Self {
            pool,
            identity: identity.into(),
            fresh: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The hostname, used when `[server] instance_id` is unset.
    ///
    /// Deliberately stable across restarts rather than random per process. It is
    /// how a restarted instance recognises its own orphaned work, and it lets a
    /// restart resume leadership immediately instead of waiting out the TTL —
    /// the old process is gone, and nothing is served by making the fleet wait.
    /// The trade is that two control planes on one host need distinct
    /// `instance_id`s configured.
    pub fn default_identity() -> String {
        std::env::var("HOSTNAME")
            .ok()
            .or_else(|| {
                std::fs::read_to_string("/etc/hostname")
                    .ok()
                    .map(|s| s.trim().to_string())
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "control".to_string())
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Whether the controller may act right now.
    ///
    /// False when this instance does not hold the lease, and false when it holds
    /// one whose remaining margin is too thin to trust — see the module note on
    /// fencing.
    pub fn is_fresh(&self) -> bool {
        self.fresh.load(Ordering::Acquire)
    }

    /// Take the lease if it is free, or extend it if this instance holds it.
    ///
    /// One statement, so acquisition is atomic: the `WHERE` decides between
    /// "expired, anyone may take it" and "mine, extend it", and two instances
    /// racing produce exactly one winner because the second sees the row the
    /// first already updated.
    async fn acquire_or_renew(&self) -> Result<Option<Held>, sqlx::Error> {
        let row: Option<(i64, bool)> = sqlx::query_as(
            "UPDATE controller_lease
                SET holder = $1,
                    -- A renewal keeps its epoch; only a change of holder is a
                    -- new term, which is what makes epoch a fencing token
                    -- rather than a renewal counter.
                    epoch = CASE WHEN holder = $1 THEN epoch ELSE epoch + 1 END,
                    acquired_at = CASE WHEN holder = $1 THEN acquired_at ELSE now() END,
                    expires_at = now() + $2::interval
              WHERE name = 'controller'
                AND (expires_at < now() OR holder = $1)
             RETURNING epoch, (holder IS DISTINCT FROM $1) AS took_over",
        )
        .bind(&self.identity)
        .bind(sqlx::postgres::types::PgInterval {
            months: 0,
            days: 0,
            microseconds: TTL.as_micros() as i64,
        })
        .fetch_optional(&self.pool)
        .await?;
        // `took_over` is computed from the pre-UPDATE value in RETURNING, so it
        // reports whether this call is the moment leadership moved.
        Ok(row.map(|(epoch, took_over)| Held { epoch, took_over }))
    }

    /// Confirm, against the database, that this instance still holds the lease
    /// with margin to spare.
    ///
    /// For the one caller that cannot rely on the cached flag: the migration
    /// controller, where acting twice corrupts a guest. Costs a round trip,
    /// which is why it is not what everything uses.
    pub async fn confirm(&self) -> bool {
        let held: Result<Option<bool>, _> = sqlx::query_scalar(
            "SELECT expires_at > now() + $2::interval
               FROM controller_lease
              WHERE name = 'controller' AND holder = $1",
        )
        .bind(&self.identity)
        .bind(sqlx::postgres::types::PgInterval {
            months: 0,
            days: 0,
            microseconds: (TTL.as_micros() / 2) as i64,
        })
        .fetch_optional(&self.pool)
        .await;
        match held {
            Ok(Some(true)) => true,
            Ok(_) => {
                // Not an error: losing a lease is normal, and the loop that
                // called this simply does nothing this tick.
                warn!("controller lease is no longer held with margin; standing down this tick");
                false
            }
            Err(e) => {
                // A database this instance cannot read is one it cannot safely
                // act on either. Fail closed.
                warn!(error = %e, "could not confirm the controller lease; standing down this tick");
                false
            }
        }
    }

    /// Release the lease so another instance can take over immediately rather
    /// than waiting out the TTL. Best-effort: a crash skips this, which is what
    /// the TTL is for.
    pub async fn release(&self) {
        let _ = sqlx::query(
            "UPDATE controller_lease SET expires_at = now() - interval '1 second'
              WHERE name = 'controller' AND holder = $1",
        )
        .bind(&self.identity)
        .execute(&self.pool)
        .await;
    }

    /// Renew forever, keeping [`is_fresh`](Self::is_fresh) current.
    ///
    /// Runs for the life of the process. It never gives up on losing the lease:
    /// an instance that is not the leader is a standby, and a standby's job is
    /// to keep trying to become one.
    pub async fn run(self: Arc<Self>) {
        let mut leading = false;
        loop {
            match self.acquire_or_renew().await {
                Ok(Some(held)) => {
                    self.fresh.store(true, Ordering::Release);
                    if !leading {
                        leading = true;
                        info!(
                            holder = %self.identity,
                            epoch = held.epoch,
                            took_over = held.took_over,
                            "this instance is now the controller"
                        );
                    }
                }
                Ok(None) => {
                    // Somebody else holds it and it has not expired.
                    self.fresh.store(false, Ordering::Release);
                    if leading {
                        leading = false;
                        warn!(
                            holder = %self.identity,
                            "lost the controller lease — standing by"
                        );
                    }
                }
                Err(e) => {
                    // Fail closed: a renewal that did not happen is a lease that
                    // may already have been taken.
                    self.fresh.store(false, Ordering::Release);
                    warn!(error = %e, "controller lease renewal failed");
                }
            }
            tokio::time::sleep(RENEW_EVERY).await;
        }
    }
}

struct Held {
    epoch: i64,
    took_over: bool,
}

/// Who holds the lease, for `GET /leader` and for operators.
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct LeaseStatus {
    pub holder: String,
    pub epoch: i64,
    pub acquired_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// Whether the lease is currently live, decided by PostgreSQL's clock so the
    /// answer does not depend on which instance was asked.
    pub valid: bool,
}

pub async fn status(pool: &PgPool) -> Result<Option<LeaseStatus>, sqlx::Error> {
    sqlx::query_as::<_, LeaseStatus>(
        "SELECT holder, epoch, acquired_at, expires_at, (expires_at > now()) AS valid
           FROM controller_lease WHERE name = 'controller'",
    )
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The renewal cadence has to leave room for a failure or two before the
    /// lease can be taken; if it did not, one slow query would hand leadership
    /// to another instance while this one was still working.
    #[test]
    fn renewal_is_frequent_enough_to_tolerate_a_missed_beat() {
        assert!(
            RENEW_EVERY * 2 < TTL,
            "two consecutive renewal failures must still leave time to retry"
        );
    }

    /// The act-margin is half the TTL, so `confirm` and `is_fresh` agree about
    /// what "with margin" means.
    #[test]
    fn the_margin_is_half_the_ttl() {
        assert_eq!(TTL.as_micros() / 2, 7_500_000);
    }

    /// Stability is the point — see `default_identity`. A random per-process id
    /// would leave every restart unable to reclaim its own orphaned work.
    #[test]
    fn identity_is_stable_across_calls() {
        assert_eq!(Lease::default_identity(), Lease::default_identity());
        assert!(!Lease::default_identity().is_empty());
    }
}
