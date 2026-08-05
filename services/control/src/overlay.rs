//! Overlay encapsulation overhead, and the guest MTU that follows from it
//! (design §18, M18b).
//!
//! A guest on a VXLAN overlay must use a smaller MTU than the underlay, because
//! every frame is wrapped before it goes on the wire. Encrypting the underlay
//! wraps it again, so the guest MTU has to shrink further — and that is a trap
//! worth spelling out:
//!
//! **The MTU must be rolled out before encryption is enabled.** It is rendered
//! into cloud-init at seed time, so a *running* VM never picks up a new value.
//! Turning on IPsec first leaves every existing overlay VM with an MTU 34 bytes
//! too large: ARP works, ping works, the TCP handshake works, and then the first
//! full-size segment vanishes. The ICMP "fragmentation needed" that would
//! normally fix this is addressed to the guest's overlay IP and emitted by the
//! host stack, which has no route onto the overlay bridge — so the guest never
//! learns. It is a silent blackhole.
//!
//! [`OverlayEncryption`] therefore has a deliberate intermediate state:
//! `Reserve` shrinks the MTU without enabling encryption, so an operator can
//! roll it out, reboot or re-seed guests, verify, and only then switch to
//! `Ipsec`.

use serde::{Deserialize, Serialize};

/// VXLAN encapsulation: 20 (outer IP) + 8 (UDP) + 8 (VXLAN) + 14 (inner
/// Ethernet header the guest must fit inside the tunnel).
pub const VXLAN_OVERHEAD: u32 = 50;

/// ESP in transport mode with AES-GCM-16, which is the proposal OVS pins:
/// 8 (ESP header) + 8 (IV) + 16 (ICV) + 2 (trailer) = 34.
pub const ESP_OVERHEAD: u32 = 34;

/// A further 8 bytes when ESP is wrapped in UDP for NAT traversal.
pub const NAT_T_OVERHEAD: u32 = 8;

/// Whether the VXLAN underlay is encrypted, and whether the guest MTU has been
/// shrunk in preparation for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayEncryption {
    /// Tunnels are cleartext. Anyone on the underlay can read overlay traffic,
    /// and anyone who can reach UDP/4789 can inject into any VNI.
    #[default]
    None,
    /// Still cleartext, but the guest MTU already leaves room for ESP. The step
    /// that makes enabling encryption safe on a cluster with running VMs.
    Reserve,
    /// Tunnels are IPsec-protected between host pairs.
    Ipsec,
}

impl OverlayEncryption {
    /// Whether ESP headroom is reserved in the guest MTU.
    pub fn reserves_headroom(self) -> bool {
        matches!(self, OverlayEncryption::Reserve | OverlayEncryption::Ipsec)
    }

    /// Whether tunnels are actually protected.
    pub fn is_encrypted(self) -> bool {
        matches!(self, OverlayEncryption::Ipsec)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OverlayEncryption::None => "none",
            OverlayEncryption::Reserve => "reserve",
            OverlayEncryption::Ipsec => "ipsec",
        }
    }
}

/// The MTU to render into an overlay NIC's guest configuration.
///
/// `underlay_mtu` is the MTU of the host link the tunnels traverse — 1500 on a
/// standard network, larger on a jumbo-frame underlay, where the old hardcoded
/// 1450 was leaving most of the frame unused.
pub fn guest_mtu(underlay_mtu: u32, encryption: OverlayEncryption, nat_traversal: bool) -> u32 {
    let mut overhead = VXLAN_OVERHEAD;
    if encryption.reserves_headroom() {
        overhead += ESP_OVERHEAD;
        if nat_traversal {
            overhead += NAT_T_OVERHEAD;
        }
    }
    underlay_mtu.saturating_sub(overhead)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value shipped before encryption existed, and still correct when it
    /// is off — so enabling nothing changes nothing.
    #[test]
    fn unencrypted_overlay_keeps_1450() {
        assert_eq!(guest_mtu(1500, OverlayEncryption::None, false), 1450);
    }

    /// Hand-checked against the wire: guest 1416 → inner Ethernet 1430 → +8
    /// VXLAN +8 UDP = 1446 plaintext → +2 trailer = 1448 (already 4-aligned) →
    /// 20 outer IP + 8 ESP + 8 IV + 1448 + 16 ICV = exactly 1500.
    #[test]
    fn ipsec_overlay_is_1416_on_a_1500_underlay() {
        assert_eq!(guest_mtu(1500, OverlayEncryption::Ipsec, false), 1416);
    }

    /// The whole point of the intermediate state: identical MTU to `Ipsec`, so
    /// the guest-visible change can be rolled out and verified on its own.
    #[test]
    fn reserve_matches_ipsec_so_the_mtu_rollout_is_separable() {
        assert_eq!(
            guest_mtu(1500, OverlayEncryption::Reserve, false),
            guest_mtu(1500, OverlayEncryption::Ipsec, false),
        );
        assert!(OverlayEncryption::Reserve.reserves_headroom());
        // ...but it does not claim to protect anything.
        assert!(!OverlayEncryption::Reserve.is_encrypted());
    }

    #[test]
    fn nat_traversal_costs_a_further_eight_bytes() {
        assert_eq!(guest_mtu(1500, OverlayEncryption::Ipsec, true), 1408);
    }

    /// A jumbo underlay is where the old hardcoded 1450 was worst: it left
    /// ~7.5 KB of every frame unused.
    #[test]
    fn a_jumbo_underlay_scales() {
        assert_eq!(guest_mtu(9000, OverlayEncryption::None, false), 8950);
        assert_eq!(guest_mtu(9000, OverlayEncryption::Ipsec, false), 8916);
    }

    /// Never underflow into an absurd MTU on a misconfigured underlay.
    #[test]
    fn a_tiny_underlay_saturates_rather_than_wrapping() {
        assert_eq!(guest_mtu(40, OverlayEncryption::Ipsec, false), 0);
    }

    #[test]
    fn encryption_is_off_by_default() {
        let e = OverlayEncryption::default();
        assert_eq!(e, OverlayEncryption::None);
        assert!(!e.reserves_headroom() && !e.is_encrypted());
    }
}
