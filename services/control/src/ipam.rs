//! Control-plane IP address management (design M13a).
//!
//! A network may carry an IPv4 and/or IPv6 subnet. When it does, the control
//! plane allocates a static address per NIC per family (instead of relying on
//! external DHCP) and renders a cloud-init **netplan v2** `network-config` that
//! pins each address to its NIC by MAC. A network with no subnet stays
//! unmanaged (DHCP), so both models coexist per network.
//!
//! Allocation is deterministic-lowest-free within the pool, skipping the network
//! address, the v4 broadcast, and the gateway. Addresses are persisted in
//! `ip_allocations`, so this module is pure: it takes the set of already-taken
//! addresses and returns the next free one.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;

/// A parsed per-family subnet: the CIDR plus an optional gateway and pool bounds.
#[derive(Debug, Clone)]
pub struct Subnet {
    pub net: IpNet,
    pub gateway: Option<IpAddr>,
    pub pool_start: Option<IpAddr>,
    pub pool_end: Option<IpAddr>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IpamError {
    #[error("invalid {what}: {value}")]
    Parse { what: &'static str, value: String },
    #[error("address {0} is not inside subnet {1}")]
    OutOfRange(IpAddr, IpNet),
    #[error("address {0} is reserved (network, broadcast, or gateway)")]
    Reserved(IpAddr),
    #[error("no free address left in subnet {0}")]
    Exhausted(IpNet),
}

fn parse<T: std::str::FromStr>(what: &'static str, value: &str) -> Result<T, IpamError> {
    value.trim().parse().map_err(|_| IpamError::Parse {
        what,
        value: value.to_string(),
    })
}

fn to_u128(ip: IpAddr) -> u128 {
    match ip {
        IpAddr::V4(a) => u32::from(a) as u128,
        IpAddr::V6(a) => u128::from(a),
    }
}

fn from_u128(v: u128, v6: bool) -> IpAddr {
    if v6 {
        IpAddr::V6(Ipv6Addr::from(v))
    } else {
        IpAddr::V4(Ipv4Addr::from(v as u32))
    }
}

impl Subnet {
    /// Parse a family's subnet from its stored string columns. `cidr` is
    /// required; the rest are optional. Returns `Ok(None)` when `cidr` is absent
    /// (that family is unmanaged).
    pub fn parse_opt(
        cidr: Option<&str>,
        gateway: Option<&str>,
        pool_start: Option<&str>,
        pool_end: Option<&str>,
    ) -> Result<Option<Self>, IpamError> {
        let Some(cidr) = cidr.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        let net: IpNet = parse("cidr", cidr)?;
        let opt = |what, v: Option<&str>| -> Result<Option<IpAddr>, IpamError> {
            match v.map(str::trim).filter(|s| !s.is_empty()) {
                Some(s) => Ok(Some(parse(what, s)?)),
                None => Ok(None),
            }
        };
        Ok(Some(Self {
            net,
            gateway: opt("gateway", gateway)?,
            pool_start: opt("pool_start", pool_start)?,
            pool_end: opt("pool_end", pool_end)?,
        }))
    }

    pub fn is_v6(&self) -> bool {
        matches!(self.net, IpNet::V6(_))
    }

    pub fn prefix_len(&self) -> u8 {
        self.net.prefix_len()
    }

    /// The [lo, hi] numeric bounds of the usable pool: the whole subnet minus
    /// the network/broadcast reserved addresses, clamped to any pool overrides.
    fn bounds(&self) -> (u128, u128) {
        let base = to_u128(self.net.network());
        let last = to_u128(self.net.broadcast());
        // Skip the network address (v4 & v6) and the v4 broadcast.
        let (mut lo, mut hi) = if self.is_v6() {
            (base.saturating_add(1), last)
        } else {
            (base.saturating_add(1), last.saturating_sub(1))
        };
        if let Some(s) = self.pool_start {
            lo = lo.max(to_u128(s));
        }
        if let Some(e) = self.pool_end {
            hi = hi.min(to_u128(e));
        }
        (lo, hi)
    }

    /// Lowest free address in the pool, skipping the gateway and `taken`.
    pub fn next_free(&self, taken: &HashSet<IpAddr>) -> Result<IpAddr, IpamError> {
        let (lo, hi) = self.bounds();
        let gw = self.gateway.map(to_u128);
        let v6 = self.is_v6();
        let mut v = lo;
        while v <= hi {
            if Some(v) != gw {
                let ip = from_u128(v, v6);
                if !taken.contains(&ip) {
                    return Ok(ip);
                }
            }
            v = v.saturating_add(1);
            if v == u128::MAX {
                break;
            }
        }
        Err(IpamError::Exhausted(self.net))
    }

    /// Validate an operator-requested address: in-subnet and not reserved.
    pub fn validate(&self, ip: IpAddr) -> Result<(), IpamError> {
        if self.is_v6() != ip.is_ipv6() || !self.net.contains(&ip) {
            return Err(IpamError::OutOfRange(ip, self.net));
        }
        let v = to_u128(ip);
        let reserved = v == to_u128(self.net.network())
            || (!self.is_v6() && v == to_u128(self.net.broadcast()))
            || self.gateway.map(to_u128) == Some(v);
        if reserved {
            return Err(IpamError::Reserved(ip));
        }
        Ok(())
    }
}

/// One NIC's rendered netplan entry.
pub struct NicRender {
    /// Deterministic interface name (eth0, eth1, …), matched to the MAC below.
    pub set_name: String,
    pub mac: String,
    /// Static addresses as "ip/prefix"; empty means DHCP on this NIC.
    pub addresses: Vec<String>,
    pub gateway4: Option<String>,
    pub gateway6: Option<String>,
    pub dns: Vec<String>,
}

/// Render a cloud-init (netplan v2) `network-config`. NICs with `addresses` get
/// a static config pinned by MAC; NICs without fall back to DHCP so mixed
/// managed/unmanaged NICs on one VM both come up.
pub fn render_network_config(nics: &[NicRender]) -> String {
    let mut s = String::from("version: 2\nethernets:\n");
    for nic in nics {
        s.push_str(&format!("  {}:\n", nic.set_name));
        s.push_str("    match:\n");
        s.push_str(&format!("      macaddress: \"{}\"\n", nic.mac));
        s.push_str(&format!("    set-name: {}\n", nic.set_name));
        if nic.addresses.is_empty() {
            s.push_str("    dhcp4: true\n");
            continue;
        }
        s.push_str("    dhcp4: false\n");
        s.push_str("    dhcp6: false\n");
        s.push_str("    addresses:\n");
        for a in &nic.addresses {
            s.push_str(&format!("      - {a}\n"));
        }
        let mut routes = String::new();
        if let Some(gw) = &nic.gateway4 {
            routes.push_str(&format!("      - to: default\n        via: {gw}\n"));
        }
        if let Some(gw) = &nic.gateway6 {
            routes.push_str(&format!("      - to: default\n        via: {gw}\n"));
        }
        if !routes.is_empty() {
            s.push_str("    routes:\n");
            s.push_str(&routes);
        }
        if !nic.dns.is_empty() {
            s.push_str("    nameservers:\n      addresses: [");
            s.push_str(&nic.dns.join(", "));
            s.push_str("]\n");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4() -> Subnet {
        Subnet::parse_opt(Some("192.168.10.0/24"), Some("192.168.10.1"), None, None)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn parse_opt_none_when_no_cidr() {
        assert!(Subnet::parse_opt(None, None, None, None).unwrap().is_none());
        assert!(Subnet::parse_opt(Some(""), None, None, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn allocates_lowest_free_skipping_network_and_gateway() {
        let s = v4();
        // .0 = network, .1 = gateway -> first free is .2
        let ip = s.next_free(&HashSet::new()).unwrap();
        assert_eq!(ip, "192.168.10.2".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn allocation_skips_taken() {
        let s = v4();
        let mut taken = HashSet::new();
        taken.insert("192.168.10.2".parse().unwrap());
        taken.insert("192.168.10.3".parse().unwrap());
        assert_eq!(
            s.next_free(&taken).unwrap(),
            "192.168.10.4".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn respects_pool_bounds() {
        let s = Subnet::parse_opt(
            Some("192.168.10.0/24"),
            Some("192.168.10.1"),
            Some("192.168.10.100"),
            Some("192.168.10.101"),
        )
        .unwrap()
        .unwrap();
        let mut taken = HashSet::new();
        assert_eq!(
            s.next_free(&taken).unwrap(),
            "192.168.10.100".parse::<IpAddr>().unwrap()
        );
        taken.insert("192.168.10.100".parse().unwrap());
        assert_eq!(
            s.next_free(&taken).unwrap(),
            "192.168.10.101".parse::<IpAddr>().unwrap()
        );
        taken.insert("192.168.10.101".parse().unwrap());
        assert_eq!(s.next_free(&taken), Err(IpamError::Exhausted(s.net)));
    }

    #[test]
    fn validate_rejects_out_of_range_and_reserved() {
        let s = v4();
        assert!(s.validate("192.168.10.50".parse().unwrap()).is_ok());
        assert!(matches!(
            s.validate("10.0.0.5".parse().unwrap()),
            Err(IpamError::OutOfRange(..))
        ));
        assert!(matches!(
            s.validate("192.168.10.1".parse().unwrap()), // gateway
            Err(IpamError::Reserved(_))
        ));
        assert!(matches!(
            s.validate("192.168.10.0".parse().unwrap()), // network
            Err(IpamError::Reserved(_))
        ));
        assert!(matches!(
            s.validate("192.168.10.255".parse().unwrap()), // broadcast
            Err(IpamError::Reserved(_))
        ));
    }

    #[test]
    fn ipv6_allocation_skips_anycast_and_gateway() {
        let s = Subnet::parse_opt(Some("fd00:56::/64"), Some("fd00:56::1"), None, None)
            .unwrap()
            .unwrap();
        // ::0 = subnet-router anycast (skipped), ::1 = gateway -> ::2
        assert_eq!(
            s.next_free(&HashSet::new()).unwrap(),
            "fd00:56::2".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn wrong_family_request_is_out_of_range() {
        let s = v4();
        assert!(matches!(
            s.validate("fd00::5".parse().unwrap()),
            Err(IpamError::OutOfRange(..))
        ));
    }

    #[test]
    fn renders_dual_stack_static_and_dhcp() {
        let nics = vec![
            NicRender {
                set_name: "eth0".into(),
                mac: "02:aa:bb:cc:dd:ee".into(),
                addresses: vec!["192.168.10.2/24".into(), "fd00:56::2/64".into()],
                gateway4: Some("192.168.10.1".into()),
                gateway6: Some("fd00:56::1".into()),
                dns: vec!["1.1.1.1".into()],
            },
            NicRender {
                set_name: "eth1".into(),
                mac: "02:11:22:33:44:55".into(),
                addresses: vec![],
                gateway4: None,
                gateway6: None,
                dns: vec![],
            },
        ];
        let out = render_network_config(&nics);
        assert!(out.contains("macaddress: \"02:aa:bb:cc:dd:ee\""));
        assert!(out.contains("- 192.168.10.2/24"));
        assert!(out.contains("- fd00:56::2/64"));
        assert!(out.contains("to: default"));
        assert!(out.contains("via: 192.168.10.1"));
        assert!(out.contains("via: fd00:56::1"));
        assert!(out.contains("addresses: [1.1.1.1]"));
        // The second NIC has no static addresses -> DHCP.
        assert!(out.contains("02:11:22:33:44:55"));
        assert!(out.contains("dhcp4: true"));
    }
}
