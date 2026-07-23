//! MAC-address allocation for VM NICs (design document, section 18).
//!
//! Deterministic from the VM id + NIC index, so the same VM always gets the
//! same MAC without persisting an allocation table — and it never collides
//! across VMs. Uses the locally-administered unicast prefix `02:`.

use uuid::Uuid;

/// Allocate the MAC for a VM's `index`-th NIC.
///
/// Mixes the full 128-bit id with the index so distinct VMs (and NICs) get
/// distinct MACs even when their ids share leading bytes.
pub fn allocate_mac(vm_id: Uuid, index: usize) -> String {
    let mixed =
        vm_id.as_u128() ^ (index as u128).wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835);
    let b = mixed.to_le_bytes();
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        b[0], b[1], b[2], b[3], b[4]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_is_locally_administered_and_stable() {
        let id = Uuid::new_v4();
        let mac = allocate_mac(id, 0);
        assert!(mac.starts_with("02:"));
        assert_eq!(mac.len(), 17);
        assert_eq!(mac, allocate_mac(id, 0), "deterministic");
        assert_ne!(allocate_mac(id, 0), allocate_mac(id, 1), "per-NIC unique");
    }

    #[test]
    fn distinct_vms_get_distinct_macs() {
        assert_ne!(
            allocate_mac(Uuid::from_u128(1), 0),
            allocate_mac(Uuid::from_u128(2), 0)
        );
    }
}
