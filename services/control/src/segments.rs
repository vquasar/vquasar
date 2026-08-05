//! Allocation of L2 segment identifiers (design §18, ADR-016).
//!
//! VXLAN VNIs are allocated by the control plane and are never caller-selectable
//! — a caller-chosen VNI is a way to join someone else's overlay. Allocation
//! happens in the same transaction as the network insert, so a control-plane
//! restart mid-create cannot leave an orphaned reservation.
//!
//! Release is *not* immediate reuse. The old allocator handed out `max(vni)+1`,
//! so deleting the highest network made the next network reuse its VNI — while
//! a host that had not yet torn down `vxbr<vni>` still carried a live tunnel
//! mesh for it. The new network would then silently adopt that mesh. Released
//! segments therefore sit in `quarantined` until a grace period has passed.

use chrono::{Duration, Utc};
use sqlx::PgPool;
use vquasar_model::SegmentKey;

/// Why a segment could not be allocated.
#[derive(Debug, thiserror::Error)]
pub enum SegmentError {
    #[error("no free VNI in range {start}..={end} — every value is allocated or quarantined")]
    Exhausted { start: u32, end: u32 },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// The configured VNI range and how long a released one is held back.
#[derive(Debug, Clone)]
pub struct SegmentPolicy {
    pub vni_start: u32,
    pub vni_end: u32,
    pub quarantine: std::time::Duration,
}

impl Default for SegmentPolicy {
    fn default() -> Self {
        Self {
            // Below 4096 is left to hand-configured and pre-existing overlays.
            vni_start: 4096,
            vni_end: 16_777_215,
            quarantine: std::time::Duration::from_secs(3600),
        }
    }
}

/// Allocate a VXLAN VNI inside `tx`, returning its segment key.
///
/// Takes the lowest value not already recorded, skipping quarantined ones. The
/// row is written in the caller's transaction, so two concurrent creates cannot
/// be handed the same VNI — the `UNIQUE (kind, value)` constraint is the
/// backstop if they race.
pub async fn allocate_vxlan(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    policy: &SegmentPolicy,
) -> Result<SegmentKey, SegmentError> {
    // Lock the table's allocated/quarantined rows so a concurrent allocation
    // cannot pick the same gap. Cheap: this table has one row per segment ever
    // used, and allocation is rare.
    let taken: Vec<i32> = sqlx::query_scalar(
        "SELECT value FROM network_segments
          WHERE kind = 'vxlan' AND state <> 'free'
          ORDER BY value
          FOR UPDATE",
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut next = policy.vni_start;
    for v in taken {
        let v = v as u32;
        if v < next {
            continue;
        }
        if v == next {
            next += 1;
        } else {
            break; // found a gap
        }
    }
    if next > policy.vni_end {
        return Err(SegmentError::Exhausted {
            start: policy.vni_start,
            end: policy.vni_end,
        });
    }

    let key = SegmentKey::Vxlan { vni: next };
    sqlx::query(
        "INSERT INTO network_segments (segment_key, kind, value, state, created_at)
         VALUES ($1, 'vxlan', $2, 'allocated', $3)",
    )
    .bind(key.canonical())
    .bind(next as i32)
    .bind(Utc::now())
    .execute(&mut **tx)
    .await?;
    Ok(key)
}

/// Bind an allocated segment to the network that now owns it.
pub async fn bind(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    segment_key: &str,
    network: uuid::Uuid,
) -> Result<(), SegmentError> {
    sqlx::query("UPDATE network_segments SET network_id = $2 WHERE segment_key = $1")
        .bind(segment_key)
        .bind(network)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Quarantine the segment a deleted network held.
///
/// Not freed: a host may still carry the overlay bridge and its tunnel mesh.
pub async fn release(pool: &PgPool, segment_key: &str) -> Result<(), SegmentError> {
    sqlx::query(
        "UPDATE network_segments
            SET state = 'quarantined', released_at = $2, network_id = NULL
          WHERE segment_key = $1",
    )
    .bind(segment_key)
    .bind(Utc::now())
    .execute(pool)
    .await?;
    Ok(())
}

/// Free segments whose quarantine has elapsed. Returns how many were freed.
///
/// Time-based for now. Gating this on every host confirming the overlay bridge
/// is gone is stronger and is the follow-up (it needs a desired-set RPC to the
/// agents); the grace period is what stops the immediate-reuse bug today.
pub async fn sweep_quarantine(pool: &PgPool, policy: &SegmentPolicy) -> Result<u64, SegmentError> {
    let cutoff =
        Utc::now() - Duration::from_std(policy.quarantine).unwrap_or_else(|_| Duration::hours(1));
    let n = sqlx::query(
        "DELETE FROM network_segments
          WHERE state = 'quarantined' AND released_at IS NOT NULL AND released_at < $1",
    )
    .bind(cutoff)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The allocator picks the lowest free value at or above the range start,
    /// filling gaps — but never a value still recorded, which is what makes
    /// quarantine work.
    fn next_free(taken: &[u32], start: u32) -> u32 {
        let mut next = start;
        let mut sorted: Vec<u32> = taken.to_vec();
        sorted.sort_unstable();
        for v in sorted {
            if v < next {
                continue;
            }
            if v == next {
                next += 1;
            } else {
                break;
            }
        }
        next
    }

    #[test]
    fn allocates_from_the_range_start() {
        assert_eq!(next_free(&[], 4096), 4096);
    }

    #[test]
    fn fills_the_lowest_gap() {
        assert_eq!(next_free(&[4096, 4097, 4099], 4096), 4098);
    }

    #[test]
    fn skips_contiguous_runs() {
        assert_eq!(next_free(&[4096, 4097, 4098], 4096), 4099);
    }

    /// The bug this replaces: the old allocator was `max+1`, so releasing the
    /// highest VNI made the next allocation reuse it. A quarantined value is
    /// still recorded, so it is not reused.
    #[test]
    fn a_quarantined_value_is_not_handed_out_again() {
        // 4096 released (quarantined, still present), 4097 live.
        assert_eq!(next_free(&[4096, 4097], 4096), 4098);
    }

    #[test]
    fn values_below_the_range_start_are_ignored() {
        // Hand-configured legacy overlays below 4096 do not shift allocation.
        assert_eq!(next_free(&[10, 100], 4096), 4096);
    }

    #[test]
    fn default_policy_reserves_below_4096() {
        let p = SegmentPolicy::default();
        assert_eq!(p.vni_start, 4096);
        assert_eq!(p.vni_end, 16_777_215);
        assert_eq!(p.quarantine.as_secs(), 3600);
    }
}
