//! Host firewall for VXLAN ingress (design §18, M18c).
//!
//! IPsec protects the host pairs it is configured for. It does **not** stop a
//! third party: a cleartext VXLAN packet from an *unconfigured* source IP
//! matches no XFRM policy, so the kernel hands it to the VXLAN socket and the
//! frame lands on the overlay bridge. Anyone who can reach UDP/4789 can
//! therefore inject into any VNI, encrypted underlay or not. Verified: the
//! inbound policy check only drops cleartext from peers we *have* a policy for.
//!
//! Closing it needs a host ingress filter, which is the one piece of this that
//! is not OVS. We use nftables directly — available on every distribution we
//! target, unlike `iptables`, which RHEL-family hosts are moving away from —
//! and keep everything in a table of our own so the operator's rules are never
//! touched. Removing the feature is `nft delete table`, nothing else.

use tokio::process::Command;

use crate::network::NetworkError;

type Result<T> = std::result::Result<T, NetworkError>;

/// Our own table. Anything vquasar adds to the host firewall lives here, so it
/// can be inspected and removed as a unit.
pub const TABLE: &str = "vquasar";

/// The ruleset: accept VXLAN that arrived under IPsec, drop the rest.
///
/// `meta ipsec exists` matches packets carrying an XFRM secpath — i.e. ones
/// that were decapsulated from ESP. A negative priority puts this ahead of a
/// distribution firewall's own input chain, and the chain policy stays `accept`
/// so nothing else on the host is affected.
pub fn ruleset() -> String {
    format!(
        "table inet {TABLE} {{\n\
         \x20 chain vxlan_ingress {{\n\
         \x20   type filter hook input priority -10; policy accept;\n\
         \x20   udp dport 4789 meta ipsec exists accept\n\
         \x20   udp dport 4789 drop\n\
         \x20 }}\n\
         }}\n"
    )
}

/// Install the filter, replacing any previous version atomically.
pub async fn apply() -> Result<()> {
    // Delete-then-add in one transaction: nft applies the whole file or none of
    // it, so there is no window where VXLAN is unfiltered *or* fully blocked.
    let script = format!(
        "table inet {TABLE} {{}}\ndelete table inet {TABLE}\n{}",
        ruleset()
    );
    run_nft(&script).await
}

/// Remove the filter (underlay encryption turned off).
///
/// Leaving it in place would silently break every overlay, because without
/// IPsec no VXLAN packet carries a secpath and all of it would be dropped.
pub async fn clear() -> Result<()> {
    let script = format!("table inet {TABLE} {{}}\ndelete table inet {TABLE}\n");
    run_nft(&script).await
}

async fn run_nft(script: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| NetworkError::Command {
            cmd: "nft -f -".into(),
            stderr: format!("{e} (is nftables installed?)"),
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script.as_bytes()).await;
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| NetworkError::Command {
            cmd: "nft -f -".into(),
            stderr: e.to_string(),
        })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(NetworkError::Command {
            cmd: "nft -f -".into(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_comes_before_drop() {
        let r = ruleset();
        let accept = r.find("accept\n").expect("no accept rule");
        let drop = r.find("drop").expect("no drop rule");
        assert!(accept < drop, "the drop would shadow the accept:\n{r}");
    }

    /// The filter must only ever touch VXLAN. A rule without the port match
    /// would take the host off the network.
    #[test]
    fn every_rule_is_scoped_to_the_vxlan_port() {
        for line in ruleset().lines() {
            let l = line.trim();
            if l.ends_with("accept") || l.ends_with("drop") {
                if l.starts_with("type filter") || l.starts_with("policy") {
                    continue;
                }
                assert!(l.contains("udp dport 4789"), "unscoped rule: {l}");
            }
        }
    }

    #[test]
    fn the_chain_default_is_accept_so_other_traffic_is_untouched() {
        assert!(ruleset().contains("policy accept"));
    }

    /// Everything lives in one table so it can be removed as a unit and never
    /// collides with the operator's own rules.
    #[test]
    fn rules_live_in_our_own_table() {
        assert!(ruleset().starts_with(&format!("table inet {TABLE}")));
        assert_eq!(ruleset().matches("table ").count(), 1);
    }

    /// Priority must be negative so we see the packet before a distribution
    /// firewall's input chain (firewalld installs at priority 0).
    #[test]
    fn runs_ahead_of_the_distribution_firewall() {
        assert!(ruleset().contains("priority -10"));
    }
}
