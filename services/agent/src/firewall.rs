//! Per-NIC stateful security groups on Open vSwitch (design M13c).
//!
//! When a NIC has security groups, the agent programs a conntrack firewall on
//! its TAP: default-deny ingress, and always allow the return traffic of
//! established/related connections. ARP, DHCP and ICMPv6 (neighbour discovery)
//! are permitted so basic L2/IPAM keeps working.
//!
//! Egress is default-*allow* unless the platform says otherwise
//! ([`Policy::egress_default_deny`]). That asymmetry is deliberate and not a
//! judgement about which direction matters: turning a running fleet to
//! default-deny egress cuts every guest off from everything it was reaching,
//! including its own package mirrors and DNS. It is an operator decision, taken
//! once, not a default that arrives with an upgrade.
//!
//! With it on, a guest may originate only what its groups' egress rules allow —
//! which is what stops a compromised guest in one tenant from reaching the
//! management underlay, the control plane, or another tenant's provider
//! network.
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

/// One resolved allow-rule. The direction is the list it lives in.
#[derive(Debug, Clone)]
pub struct SecRule {
    pub ipv6: bool,
    /// tcp | udp | icmp | any
    pub protocol: String,
    pub port_min: u16,
    pub port_max: u16,
    /// The remote end: the source on ingress, the destination on egress.
    /// Empty ⇒ any.
    pub remote_cidr: String,
}

/// The resolved policy for one NIC.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    pub ingress: Vec<SecRule>,
    /// Only meaningful with `egress_default_deny`: under default-allow every
    /// egress rule is a no-op, so the control plane refuses to record one
    /// rather than accept a rule that does nothing.
    pub egress: Vec<SecRule>,
    pub egress_default_deny: bool,
}

/// Which end of a rule the remote CIDR describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    /// Remote is the source, and the packet is addressed to this NIC.
    Ingress,
    /// Remote is the destination, and the packet comes from this NIC.
    Egress,
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
fn rule_matches(r: &SecRule, dir: Dir) -> Vec<String> {
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
    // The remote end is the source of an inbound packet and the destination of
    // an outbound one. Matching `nw_src` on an egress rule would be asking
    // where the guest's own packet came from, which is always the guest.
    let cidr = r.remote_cidr.trim();
    let src = match (cidr.is_empty(), dir, r.ipv6) {
        (true, _, _) => String::new(),
        (false, Dir::Ingress, false) => format!(",nw_src={cidr}"),
        (false, Dir::Ingress, true) => format!(",ipv6_src={cidr}"),
        (false, Dir::Egress, false) => format!(",nw_dst={cidr}"),
        (false, Dir::Egress, true) => format!(",ipv6_dst={cidr}"),
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

/// Priority of the catch-all drop that ends every TAP's egress pipeline.
///
/// Below every legitimate egress match (which are all ≥1080 and carry
/// `dl_src=<mac>`), and above the bridge's default `NORMAL` flow. A frame the
/// guest sources with someone else's MAC matches nothing above this and dies
/// here.
const SPOOF_DROP_PRIORITY: u32 = 1000;

/// Port security for a NIC with **no** security groups (design §30).
///
/// The guest still reaches everything it reaches today — this only binds its
/// egress to the MAC the control plane allocated. Without it a guest can source
/// frames as any MAC on the shared bridge and impersonate another VM, which no
/// amount of control-plane scoping can undo.
///
/// Deliberately not a conntrack policy: filtering is the security groups' job
/// (M13c), and turning "no groups" into "default deny" would cut off every
/// existing NIC. This is the part that is safe to apply unconditionally.
pub fn build_port_security_flows(tap: &str, mac: &str) -> Vec<String> {
    let c = cookie_for(tap);
    vec![
        // ARP must carry the guest's own MAC in both the frame and the sender
        // hardware address, or it is an ARP-poisoning attempt.
        format!(
            "cookie={c},table=0,priority=1200,in_port={tap},dl_src={mac},arp,arp_sha={mac},actions=NORMAL"
        ),
        // Everything else the guest sends, as long as it owns the source MAC.
        format!("cookie={c},table=0,priority=1180,in_port={tap},dl_src={mac},actions=NORMAL"),
        // Anything else out of this port is spoofed.
        format!("cookie={c},table=0,priority={SPOOF_DROP_PRIORITY},in_port={tap},actions=drop"),
    ]
}

/// Build the full flow set for a NIC's firewall (pure — unit-tested).
pub fn build_flows(tap: &str, mac: &str, policy: &Policy) -> Vec<String> {
    let rules = &policy.ingress;
    let c = cookie_for(tap);
    let z = zone_for(tap);
    let ct = format!("ct(table={RESULT_TABLE},zone={z})");
    let commit = format!("ct(commit,zone={z}),NORMAL");
    let mut f = Vec::new();

    // --- table 0: allow essentials, send IP to conntrack ---
    // Egress is qualified with `dl_src` so a spoofed source MAC misses every
    // rule here and falls through to the catch-all drop below; ingress keys on
    // `dl_dst`, which is already this NIC's MAC.
    for (dir, dir_match) in [
        ("eg", format!("in_port={tap},dl_src={mac}")),
        ("ig", format!("dl_dst={mac}")),
    ] {
        // On egress `dir_match` already pins dl_src; also pin the ARP sender
        // hardware address, or a guest can poison peers while sourcing frames
        // from its own MAC.
        let arp_extra = if dir == "eg" {
            format!(",arp_sha={mac}")
        } else {
            String::new()
        };
        f.push(format!(
            "cookie={c},table=0,priority=1100,{dir_match},arp{arp_extra},actions=NORMAL"
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
        "cookie={c},table=0,priority=1090,in_port={tap},dl_src={mac},udp,tp_src=68,tp_dst=67,actions=NORMAL"
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
    if policy.egress_default_deny {
        // Only what the groups allow may leave. Each rule is matched on the
        // *destination*, and anything else new from this NIC dies at the drop
        // below.
        for r in &policy.egress {
            for m in rule_matches(r, Dir::Egress) {
                f.push(format!(
                    "cookie={c},table={RESULT_TABLE},priority=90,ct_zone={z},ct_state=+new+trk,in_port={tap},dl_src={mac},{m},actions={commit}"
                ));
            }
        }
        // Sits above the ingress default-deny only so the two are readable as a
        // pair; they cannot both match one packet (this one keys on `in_port`,
        // that one on `dl_dst`).
        f.push(format!(
            "cookie={c},table={RESULT_TABLE},priority=20,ct_zone={z},ct_state=+new+trk,in_port={tap},dl_src={mac},actions=drop"
        ));
    } else {
        // New egress from the VM is allowed (default-allow egress), commit it.
        // A ct(commit) action requires a known dl_type, so match ip / ipv6
        // explicitly. `dl_src` is redundant here — nothing reaches this table
        // without passing the table-0 egress rules, which pin it — but it keeps
        // "every egress match names the MAC" true no matter how table 0 is
        // edited later.
        for l3 in ["ip", "ipv6"] {
            f.push(format!(
                "cookie={c},table={RESULT_TABLE},priority=90,ct_zone={z},ct_state=+new+trk,in_port={tap},dl_src={mac},{l3},actions={commit}"
            ));
        }
    }
    // New ingress: only where an allow-rule matches.
    for r in rules {
        for m in rule_matches(r, Dir::Ingress) {
            f.push(format!(
                "cookie={c},table={RESULT_TABLE},priority=80,ct_zone={z},ct_state=+new+trk,dl_dst={mac},{m},actions={commit}"
            ));
        }
    }
    // Default-deny any other new ingress to this NIC.
    f.push(format!(
        "cookie={c},table={RESULT_TABLE},priority=10,ct_zone={z},ct_state=+new+trk,dl_dst={mac},actions=drop"
    ));
    // Port security: anything this guest sources with a MAC that is not its own
    // matched none of the egress rules above (they all pin `dl_src`), so it
    // lands here (design §30).
    f.push(format!(
        "cookie={c},table=0,priority={SPOOF_DROP_PRIORITY},in_port={tap},actions=drop"
    ));
    f
}

/// Install the firewall for a NIC's TAP on `bridge`.
pub async fn apply(bridge: &str, tap: &str, mac: &str, policy: &Policy) -> Result<()> {
    // Replace any prior flows for this TAP first (idempotent reconcile).
    clear(bridge, tap).await?;
    let flows = build_flows(tap, mac, policy).join("\n");
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

/// Install port security only, for a NIC with no security groups.
pub async fn apply_port_security(bridge: &str, tap: &str, mac: &str) -> Result<()> {
    clear(bridge, tap).await?;
    let flows = build_port_security_flows(tap, mac).join("\n");
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

    /// Today's policy: an ingress allow-list over default-allow egress.
    fn ingress_only(rules: Vec<SecRule>) -> Policy {
        Policy {
            ingress: rules,
            ..Policy::default()
        }
    }

    #[test]
    fn build_has_default_deny_and_stateful_and_allow() {
        let flows = build_flows(
            "tapabc0",
            "02:aa:bb:cc:dd:ee",
            &ingress_only(vec![rule("tcp", 22, 22, "10.0.0.0/24")]),
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
        assert_eq!(rule_matches(&r, Dir::Ingress), vec!["icmp".to_string()]);
        r.ipv6 = true;
        assert_eq!(rule_matches(&r, Dir::Ingress), vec!["icmp6".to_string()]);
        let any = rule("any", 0, 0, "192.168.0.0/16");
        assert_eq!(
            rule_matches(&any, Dir::Ingress),
            vec!["ip,nw_src=192.168.0.0/16".to_string()]
        );
    }

    /// The default has to stay default-allow egress: flipping a running fleet
    /// would cut every filtered guest off from DNS, its mirrors and everything
    /// else it reaches today.
    #[test]
    fn egress_is_allowed_unless_the_platform_says_otherwise() {
        let f = build_flows("tap0", MAC, &ingress_only(vec![])).join("\n");
        // A blanket commit of new egress, and no egress drop.
        assert!(f.contains(&format!(
            "ct_state=+new+trk,in_port=tap0,dl_src={MAC},ip,actions="
        )));
        assert!(
            !f.contains(&format!("in_port=tap0,dl_src={MAC},actions=drop")),
            "egress was denied without being asked: {f}"
        );
    }

    /// With it on, a guest may originate only what its groups allow — the
    /// property that keeps a compromised guest off the underlay.
    #[test]
    fn enforced_egress_allows_only_what_a_rule_names() {
        let policy = Policy {
            ingress: vec![],
            egress: vec![rule("tcp", 443, 443, "10.9.0.0/16")],
            egress_default_deny: true,
        };
        let f = build_flows("tap0", MAC, &policy).join("\n");
        // The allow, matched on where the packet is *going*.
        assert!(f.contains("tp_dst=443"), "{f}");
        assert!(
            f.contains("nw_dst=10.9.0.0/16"),
            "an egress rule must match the destination, not the source: {f}"
        );
        assert!(!f.contains("nw_src=10.9.0.0/16"), "{f}");
        // And the drop that makes the allow mean something.
        assert!(
            f.contains(&format!(
                "priority=20,ct_zone={},ct_state=+new+trk,in_port=tap0,dl_src={MAC},actions=drop",
                zone_for("tap0")
            )),
            "{f}"
        );
        // The blanket "commit anything new from this NIC" must be gone, or the
        // drop below it is unreachable.
        assert!(
            !f.contains(&format!(
                "ct_state=+new+trk,in_port=tap0,dl_src={MAC},ip,actions="
            )),
            "default-allow egress survived alongside the deny: {f}"
        );
    }

    /// Return traffic still flows: a default-deny egress policy that also broke
    /// replies to permitted inbound connections would look like a network fault.
    #[test]
    fn enforced_egress_still_lets_established_traffic_back_out() {
        let policy = Policy {
            ingress: vec![rule("tcp", 22, 22, "")],
            egress: vec![],
            egress_default_deny: true,
        };
        let f = build_flows("tap0", MAC, &policy).join("\n");
        assert!(f.contains("ct_state=+est+trk,actions=NORMAL"), "{f}");
        assert!(f.contains("ct_state=+rel+trk,actions=NORMAL"), "{f}");
        // ARP and DHCP are still let through, or the guest cannot use the
        // network at all.
        assert!(f.contains("arp,actions=NORMAL"), "{f}");
        assert!(f.contains("tp_dst=67,actions=NORMAL"), "{f}");
    }

    /// An egress rule with no CIDR is "anywhere on this port", not "nowhere".
    #[test]
    fn an_egress_rule_without_a_cidr_matches_any_destination() {
        let policy = Policy {
            ingress: vec![],
            egress: vec![rule("udp", 53, 53, "")],
            egress_default_deny: true,
        };
        let f = build_flows("tap0", MAC, &policy).join("\n");
        assert!(f.contains("udp,tp_dst=53,actions="), "{f}");
        assert!(!f.contains("nw_dst="), "{f}");
    }

    // ---- port security (design §30) -----------------------------------

    const MAC: &str = "02:aa:bb:cc:dd:01";
    const OTHER: &str = "02:aa:bb:cc:dd:99";

    /// Every rule that lets a frame *out* of the TAP must pin the source MAC,
    /// or the catch-all drop below it is decorative.
    fn egress_rules_all_pin_the_mac(flows: &[String]) {
        for f in flows {
            let is_egress = f.contains("in_port=tap0") && !f.contains("actions=drop");
            if is_egress {
                assert!(
                    f.contains(&format!("dl_src={MAC}")),
                    "egress rule without dl_src: {f}"
                );
            }
        }
    }

    #[test]
    fn unfiltered_nic_gets_port_security_and_a_catch_all_drop() {
        let f = build_port_security_flows("tap0", MAC);
        egress_rules_all_pin_the_mac(&f);
        assert!(
            f.iter().any(|r| r.contains(&format!(
                "priority={SPOOF_DROP_PRIORITY},in_port=tap0,actions=drop"
            ))),
            "missing catch-all drop: {f:?}"
        );
        // The guest keeps working: it is not a conntrack policy, just a bind.
        assert!(f
            .iter()
            .any(|r| r.contains("dl_src=") && r.contains("actions=NORMAL")));
        assert!(
            !f.iter().any(|r| r.contains("ct(")),
            "must not filter L3/L4: {f:?}"
        );
    }

    #[test]
    fn arp_must_carry_the_guests_own_sender_address() {
        for f in [
            build_port_security_flows("tap0", MAC),
            build_flows("tap0", MAC, &Policy::default()),
        ] {
            let arp_out: Vec<_> = f
                .iter()
                .filter(|r| r.contains("arp") && r.contains("in_port=tap0"))
                .collect();
            assert!(!arp_out.is_empty(), "no egress ARP rule");
            for r in arp_out {
                assert!(
                    r.contains(&format!("arp_sha={MAC}")),
                    "ARP egress without arp_sha, so poisoning is possible: {r}"
                );
            }
        }
    }

    /// The filtered path must not lose port security when security groups are
    /// present — the two layers are independent.
    #[test]
    fn filtered_nic_also_pins_the_mac_and_drops_the_rest() {
        let f = build_flows("tap0", MAC, &ingress_only(vec![rule("tcp", 22, 22, "")]));
        egress_rules_all_pin_the_mac(&f);
        assert!(
            f.iter().any(|r| r.contains(&format!(
                "priority={SPOOF_DROP_PRIORITY},in_port=tap0,actions=drop"
            ))),
            "filtered NIC lost the spoof drop"
        );
        // ...and the security-group policy is still there.
        assert!(f.iter().any(|r| r.contains("tp_dst=22")));
        assert!(f
            .iter()
            .any(|r| r.contains("ct_state=+new+trk") && r.contains("actions=drop")));
    }

    /// No rule may admit a frame sourced from someone else's MAC. This is the
    /// property the whole change exists for.
    #[test]
    fn no_flow_admits_another_vms_mac_on_egress() {
        for f in [
            build_port_security_flows("tap0", MAC),
            build_flows("tap0", MAC, &ingress_only(vec![rule("tcp", 0, 0, "")])),
        ] {
            for r in &f {
                if r.contains(OTHER) {
                    panic!("a foreign MAC appears in a flow: {r}");
                }
            }
            // The only rule matching bare in_port (no dl_src) is the drop.
            for r in &f {
                if r.contains("in_port=tap0") && !r.contains("dl_src=") {
                    assert!(r.contains("actions=drop"), "unqualified egress rule: {r}");
                }
            }
        }
    }

    #[test]
    fn dhcp_still_works_through_port_security() {
        let f = build_flows("tap0", MAC, &Policy::default());
        let dhcp_out = f
            .iter()
            .find(|r| r.contains("tp_src=68") && r.contains("in_port=tap0"))
            .expect("no DHCP egress rule");
        assert!(dhcp_out.contains(&format!("dl_src={MAC}")));
        assert!(f
            .iter()
            .any(|r| r.contains("tp_dst=68") && r.contains("dl_dst=")));
    }
}
