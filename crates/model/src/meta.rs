//! Common metadata carried by every persistent resource (design document,
//! section 6).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A monotonically increasing revision counter.
///
/// `generation` is bumped whenever a resource's *desired* state (`spec`)
/// changes. A controller records the `generation` it last acted on as
/// `observed_generation`, so `spec.generation != status.observed_generation`
/// means "reconciliation pending" (section 7). It also underpins optimistic
/// concurrency for persisted updates (section 31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Generation(pub i64);

impl Generation {
    /// The generation of a freshly created resource.
    pub const INITIAL: Generation = Generation(1);

    /// Return the next generation.
    pub const fn next(self) -> Generation {
        Generation(self.0 + 1)
    }
}

impl Default for Generation {
    fn default() -> Self {
        Generation::INITIAL
    }
}

/// Metadata shared by all persistent resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    /// User-friendly, mutable label (not an identity — section 23).
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub generation: Generation,
}

impl Metadata {
    /// Construct metadata for a newly created resource stamped at `now`.
    ///
    /// Time is passed in rather than read from the clock so that construction
    /// stays deterministic and testable.
    pub fn new(name: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            name: name.into(),
            created_at: now,
            updated_at: now,
            generation: Generation::INITIAL,
        }
    }

    /// Record a spec change: bump the generation and `updated_at`.
    pub fn touch_spec(&mut self, now: DateTime<Utc>) {
        self.generation = self.generation.next();
        self.updated_at = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_spec_bumps_generation() {
        let t0 = DateTime::from_timestamp(0, 0).unwrap();
        let t1 = DateTime::from_timestamp(10, 0).unwrap();
        let mut meta = Metadata::new("db-01", t0);
        assert_eq!(meta.generation, Generation::INITIAL);
        meta.touch_spec(t1);
        assert_eq!(meta.generation, Generation(2));
        assert_eq!(meta.updated_at, t1);
        assert_eq!(meta.created_at, t0);
    }
}
