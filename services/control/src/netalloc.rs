//! MAC-address allocation for VM NICs (design document, section 18).
//!
//! Deterministic from the VM id + NIC index, so the same VM always gets the
//! same MAC without persisting an allocation table — and it never collides
//! across VMs. Uses the locally-administered unicast prefix `02:`.

use uuid::Uuid;

/// Allocate the MAC for a VM's `index`-th NIC. Delegates to the shared model
/// allocator so the agent can re-derive the same MAC for IP discovery (M11).
pub fn allocate_mac(vm_id: Uuid, index: usize) -> String {
    vquasar_model::allocate_mac(vquasar_model::VmId::from_uuid(vm_id), index)
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
