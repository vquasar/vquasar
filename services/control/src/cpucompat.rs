//! CPU compatibility for live migration (design M15, cross-CPU migration).
//!
//! Cloud Hypervisor passes the host CPUID through to the guest and cannot mask
//! it to a common baseline (its only maskable x86 feature is AMX). So a guest
//! that observed a feature on its source host will fault (`#UD`) if migrated to
//! a host whose CPU lacks it. We therefore refuse a migration unless the
//! destination is CPU-compatible with the source.
//!
//! Compatibility here is deliberately conservative and asymmetric: the
//! destination must have the **same vendor** and its guest-visible ISA feature
//! set must be a **superset** of the source's. The feature sets are the curated
//! guest-ISA flags the agent reports (host/mitigation/topology noise excluded),
//! so a superset check maps directly onto "can the guest keep running there".

/// The outcome of a source→destination CPU compatibility check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpuCompat {
    /// Destination can safely run the source's guest.
    Compatible,
    /// Source and destination CPU vendors differ.
    VendorMismatch { source: String, target: String },
    /// Destination is missing guest-visible features the source has.
    MissingFeatures(Vec<String>),
    /// One side has no reported CPU inventory yet (agent hasn't heartbeat with
    /// the newer fields, or is too old). We can't prove compatibility.
    Unknown,
}

impl CpuCompat {
    /// A one-line, operator-facing explanation for an incompatible result.
    pub fn reason(&self) -> String {
        match self {
            CpuCompat::Compatible => "CPU-compatible".to_string(),
            CpuCompat::VendorMismatch { source, target } => {
                format!("CPU vendor mismatch: source is {source}, target is {target}")
            }
            CpuCompat::MissingFeatures(f) => {
                format!(
                    "target CPU is missing features the guest may use: {}",
                    f.join(", ")
                )
            }
            CpuCompat::Unknown => {
                "CPU features for the source or target host are not known yet".to_string()
            }
        }
    }
}

/// Check whether a guest running on `source` can migrate to `target`.
///
/// `*_vendor` is the `/proc/cpuinfo` `vendor_id` (may be absent on old agents);
/// `*_features` is the curated guest-ISA flag set. An empty target feature set
/// paired with a non-empty source is treated as [`CpuCompat::Unknown`] rather
/// than a spurious wall of missing features (the target simply hasn't reported).
pub fn check(
    source_vendor: Option<&str>,
    source_features: &[String],
    target_vendor: Option<&str>,
    target_features: &[String],
) -> CpuCompat {
    // If either side hasn't reported CPU inventory, don't fabricate a verdict.
    match (source_vendor, target_vendor) {
        (Some(s), Some(t)) if s != t => {
            return CpuCompat::VendorMismatch {
                source: s.to_string(),
                target: t.to_string(),
            };
        }
        (Some(_), Some(_)) => {}
        _ => return CpuCompat::Unknown,
    }
    if source_features.is_empty() || target_features.is_empty() {
        return CpuCompat::Unknown;
    }

    let target: std::collections::HashSet<&str> =
        target_features.iter().map(String::as_str).collect();
    let mut missing: Vec<String> = source_features
        .iter()
        .filter(|f| !target.contains(f.as_str()))
        .cloned()
        .collect();
    if missing.is_empty() {
        CpuCompat::Compatible
    } else {
        missing.sort();
        CpuCompat::MissingFeatures(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn identical_hosts_are_compatible() {
        let f = v(&["avx", "avx2", "sse4_2"]);
        assert_eq!(
            check(Some("GenuineIntel"), &f, Some("GenuineIntel"), &f),
            CpuCompat::Compatible
        );
    }

    #[test]
    fn superset_target_is_compatible() {
        // Skylake-SP guest -> Cascade Lake (adds avx512_vnni): safe.
        let src = v(&["avx", "avx2", "avx512f"]);
        let dst = v(&["avx", "avx2", "avx512f", "avx512_vnni", "mpx", "umip"]);
        assert!(
            check(Some("GenuineIntel"), &src, Some("GenuineIntel"), &dst) == CpuCompat::Compatible
        );
    }

    #[test]
    fn target_missing_features_is_blocked() {
        // Cascade Lake guest -> Skylake-SP (drops avx512_vnni/mpx/umip): unsafe.
        let src = v(&["avx", "avx2", "avx512f", "avx512_vnni", "mpx", "umip"]);
        let dst = v(&["avx", "avx2", "avx512f"]);
        assert_eq!(
            check(Some("GenuineIntel"), &src, Some("GenuineIntel"), &dst),
            CpuCompat::MissingFeatures(v(&["avx512_vnni", "mpx", "umip"]))
        );
    }

    #[test]
    fn different_vendor_is_blocked() {
        let f = v(&["sse2"]);
        assert!(matches!(
            check(Some("GenuineIntel"), &f, Some("AuthenticAMD"), &f),
            CpuCompat::VendorMismatch { .. }
        ));
    }

    #[test]
    fn unknown_when_features_absent() {
        let f = v(&["avx"]);
        assert_eq!(
            check(Some("GenuineIntel"), &f, Some("GenuineIntel"), &[]),
            CpuCompat::Unknown
        );
        assert_eq!(
            check(None, &f, Some("GenuineIntel"), &f),
            CpuCompat::Unknown
        );
    }
}
