//! Per-NIC stateful security groups on Open vSwitch (design M13c).
//!
//! When a NIC has security groups, the agent programs a conntrack firewall on
//! its TAP: default-deny ingress, allow egress, and always allow the return
//! traffic of established/related connections. ARP, DHCP and ICMPv6 (neighbour
//! discovery) are permitted so basic L2/IPAM keeps working. Ingress `rules` are
//! the allow-list.
//!
//! Flows live in two tables so the bridge's default `NORMAL` flow still forwards
//! everything else: table 0 classifies a NIC's traffic into conntrack, and a
//! result table (60) applies the policy. Every flow carries a per-TAP cookie so
//! release can delete exactly this NIC's flows, and a per-TAP conntrack zone so
//! connection state never bleeds between NICs.

use tokio::process::Command;

use crate::network::NetworkError;

type Result<T> = std::result::Result<T, NetworkError>;

const RESULT_TABLE: u32 = 60;

/// One resolved ingress allow-rule.
#[derive(Debug, Clone)]
pub struct SecRule {
    pub ipv6: bool,
    /// tcp | udp | icmp | any
    pub protocol: String,
    pub port_min: u16,
    pub port_max: u16,
    /// Allowed source CIDR; empty ⇒ any.
    pub remote_cidr: String,
}

/// A tiny FNV-1a hash, used for a stable per-TAP cookie and conntrack zone.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn cookie_for(tap: &str) -> u64 {
    fnv1a(tap) | 1 // non-zero
}

fn zone_for(tap: &str) -> u16 {
    // 1..=65535 (avoid the default zone 0).
    ((fnv1a(tap) % 65535) as u16) + 1
}

/// Decompose an inclusive TCP/UDP port range into `(value, mask)` pairs for
/// OpenFlow `tp_dst` matches (OVS has no native range match).
fn port_masks(min: u16, max: u16) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    let mut a = min as u32;
    let b = max as u32;
    while a <= b {
        // Largest aligned power-of-two block starting at `a` that fits in [a, b].
        let align = if a == 0 {
            1u32 << 16
        } else {
            1u32 << a.trailing_zeros()
        };
        let mut size = align;
        while a + size - 1 > b {
            size >>= 1;
        }
        let mask = (0xFFFFu32 & !(size - 1)) as u16;
        out.push((a as u16, mask));
        a += size;
        if a > 0xFFFF {
            break;
        }
    }
    out
}

/// The OVS match tokens for a rule's protocol/ports/source, or `None` if the
/// rule is malformed (skipped rather than failing the whole NIC).
fn rule_matches(r: &SecRule) -> Vec<String> {
    let proto = match (r.protocol.as_str(), r.ipv6) {
        ("tcp", false) => "tcp",
        ("tcp", true) => "tcp6",
        ("udp", false) => "udp",
        ("udp", true) => "udp6",
        ("icmp", false) => "icmp",
        ("icmp", true) => "icmp6",
        (_, false) => "ip", // "any"
        (_, true) => "ipv6",
    };
    let src = if r.remote_cidr.trim().is_empty() {
        String::new()
    } else if r.ipv6 {
        format!(",ipv6_src={}", r.remote_cidr.trim())
    } else {
        format!(",nw_src={}", r.remote_cidr.trim())
    };

    let ports = matches!(r.protocol.as_str(), "tcp" | "udp")
        && !(r.port_min == 0 && r.port_max == 0)
        && !(r.port_min == 0 && r.port_max == 0xFFFF);
    if ports {
        let (lo, hi) = if r.port_min <= r.port_max {
            (r.port_min, r.port_max)
        } else {
            (r.port_max, r.port_min)
        };
        port_masks(lo, hi)
            .into_iter()
            .map(|(v, m)| {
                if m == 0xFFFF {
                    format!("{proto},tp_dst={v}{src}")
                } else {
                    format!("{proto},tp_dst={v:#06x}/{m:#06x}{src}")
                }
            })
            .collect()
    } else {
        vec![format!("{proto}{src}")]
    }
}

/// Build the full flow set for a NIC's firewall (pure — unit-tested).
pub fn build_flows(tap: &str, mac: &str, rules: &[SecRule]) -> Vec<String> {
    let c = cookie_for(tap);
    let z = zone_for(tap);
    let ct = format!("ct(table={RESULT_TABLE},zone={z})");
    let commit = format!("ct(commit,zone={z}),NORMAL");
    let mut f = Vec::new();

    // --- table 0: allow essentials, send IP to conntrack ---
    for (dir, dir_match) in [
        ("eg", format!("in_port={tap}")),
        ("ig", format!("dl_dst={mac}")),
    ] {
        let _ = dir;
        f.push(format!(
            "cookie={c},table=0,priority=1100,{dir_match},arp,actions=NORMAL"
        ));
        f.push(format!(
            "cookie={c},table=0,priority=1085,{dir_match},icmp6,actions=NORMAL"
        ));
        f.push(format!(
            "cookie={c},table=0,priority=1080,{dir_match},ip,actions={ct}"
        ));
        f.push(format!(
            "cookie={c},table=0,priority=1080,{dir_match},ipv6,actions={ct}"
        ));
    }
    // DHCP client (so IPAM/DHCP still works through the filter).
    f.push(format!(
        "cookie={c},table=0,priority=1090,in_port={tap},udp,tp_src=68,tp_dst=67,actions=NORMAL"
    ));
    f.push(format!(
        "cookie={c},table=0,priority=1090,dl_dst={mac},udp,tp_src=67,tp_dst=68,actions=NORMAL"
    ));

    // --- result table: stateful policy ---
    f.push(format!(
        "cookie={c},table={RESULT_TABLE},priority=100,ct_zone={z},ct_state=+est+trk,actions=NORMAL"
    ));
    f.push(format!(
        "cookie={c},table={RESULT_TABLE},priority=100,ct_zone={z},ct_state=+rel+trk,actions=NORMAL"
    ));
    f.push(format!(
        "cookie={c},table={RESULT_TABLE},priority=100,ct_zone={z},ct_state=+inv+trk,actions=drop"
    ));
    // New egress from the VM is allowed (default-allow egress), commit it. A
    // ct(commit) action requires a known dl_type, so match ip / ipv6 explicitly.
    for l3 in ["ip", "ipv6"] {
        f.push(format!(
            "cookie={c},table={RESULT_TABLE},priority=90,ct_zone={z},ct_state=+new+trk,in_port={tap},{l3},actions={commit}"
        ));
    }
    // New ingress: only where an allow-rule matches.
    for r in rules {
        for m in rule_matches(r) {
            f.push(format!(
                "cookie={c},table={RESULT_TABLE},priority=80,ct_zone={z},ct_state=+new+trk,dl_dst={mac},{m},actions={commit}"
            ));
        }
    }
    // Default-deny any other new ingress to this NIC.
    f.push(format!(
        "cookie={c},table={RESULT_TABLE},priority=10,ct_zone={z},ct_state=+new+trk,dl_dst={mac},actions=drop"
    ));
    f
}

/// Install the firewall for a NIC's TAP on `bridge`.
pub async fn apply(bridge: &str, tap: &str, mac: &str, rules: &[SecRule]) -> Result<()> {
    // Replace any prior flows for this TAP first (idempotent reconcile).
    clear(bridge, tap).await?;
    let flows = build_flows(tap, mac, rules).join("\n");
    let output = ovs_ofctl_stdin(&["add-flows", bridge, "-"], &flows).await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(NetworkError::Command {
            cmd: format!("ovs-ofctl add-flows {bridge} -"),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// Remove a NIC's firewall flows (idempotent) by its per-TAP cookie.
pub async fn clear(bridge: &str, tap: &str) -> Result<()> {
    let cookie = format!("cookie={:#x}/-1", cookie_for(tap));
    for table in ["table=0", &format!("table={RESULT_TABLE}")] {
        let _ = Command::new("ovs-ofctl")
            .args(["del-flows", bridge, &format!("{cookie},{table}")])
            .output()
            .await;
    }
    Ok(())
}

/// Run `ovs-ofctl <args>` feeding `input` on stdin (for batch `add-flows -`).
async fn ovs_ofctl_stdin(args: &[&str], input: &str) -> std::io::Result<std::process::Output> {
    use tokio::io::AsyncWriteExt;
    let mut child = Command::new("ovs-ofctl")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).await?;
        stdin.shutdown().await?;
    }
    child.wait_with_output().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(proto: &str, min: u16, max: u16, cidr: &str) -> SecRule {
        SecRule {
            ipv6: false,
            protocol: proto.into(),
            port_min: min,
            port_max: max,
            remote_cidr: cidr.into(),
        }
    }

    #[test]
    fn port_masks_exact_and_range() {
        assert_eq!(port_masks(22, 22), vec![(22, 0xFFFF)]);
        // 1024..1027 = 1024/0xFFFC (a 4-block).
        assert_eq!(port_masks(1024, 1027), vec![(1024, 0xFFFC)]);
        // A non-power range decomposes into several aligned blocks covering it.
        let masks = port_masks(1000, 2000);
        for p in [1000u16, 1500, 2000] {
            assert!(
                masks.iter().any(|(v, m)| (p & m) == (v & m)),
                "port {p} not covered"
            );
        }
        assert!(!masks.iter().any(|(v, m)| (999u16 & m) == (v & m)));
        assert!(!masks.iter().any(|(v, m)| (2001u16 & m) == (v & m)));
    }

    #[test]
    fn build_has_default_deny_and_stateful_and_allow() {
        let flows = build_flows(
            "tapabc0",
            "02:aa:bb:cc:dd:ee",
            &[rule("tcp", 22, 22, "10.0.0.0/24")],
        );
        let all = flows.join("\n");
        // stateful return traffic
        assert!(all.contains("ct_state=+est+trk,actions=NORMAL"));
        // egress default allow
        assert!(all.contains("ct_state=+new+trk,in_port=tapabc0"));
        // the ssh allow rule with source restriction
        assert!(all.contains("tp_dst=22"));
        assert!(all.contains("nw_src=10.0.0.0/24"));
        // default-deny new ingress
        assert!(
            all.contains("priority=10,") && all.contains("dl_dst=02:aa:bb:cc:dd:ee,actions=drop")
        );
        // ARP + DHCP allowed
        assert!(all.contains("arp,actions=NORMAL"));
        assert!(all.contains("tp_dst=67,actions=NORMAL"));
    }

    #[test]
    fn icmp_and_any_and_ipv6_protocol_tokens() {
        let mut r = rule("icmp", 0, 0, "");
        assert_eq!(rule_matches(&r), vec!["icmp".to_string()]);
        r.ipv6 = true;
        assert_eq!(rule_matches(&r), vec!["icmp6".to_string()]);
        let any = rule("any", 0, 0, "192.168.0.0/16");
        assert_eq!(
            rule_matches(&any),
            vec!["ip,nw_src=192.168.0.0/16".to_string()]
        );
    }
}
