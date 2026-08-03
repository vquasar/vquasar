//! Agentless guest-IP discovery (design M11).
//!
//! No in-guest agent is required. The host owns `br-int`, so it can learn each
//! VM's IP by ARP/neighbor snooping: a periodic sweep of the bridge subnet
//! populates the kernel neighbor table (an ICMP echo triggers ARP resolution),
//! and we then read `ip neigh` and map MAC -> IP. VMs are matched to entries by
//! their Cloud-Hypervisor-assigned NIC MAC.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::{Mutex, Semaphore};
use tracing::debug;

/// A refreshed MAC -> IPv4 map, learned from the host neighbor table.
#[derive(Clone)]
pub struct IpDiscovery {
    bridge: String,
    cache: Arc<Mutex<HashMap<String, String>>>,
}

impl IpDiscovery {
    pub fn new(bridge: impl Into<String>) -> Self {
        Self {
            bridge: bridge.into(),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Spawn a background task that periodically refreshes the MAC->IP map.
    pub fn start(&self) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                me.refresh().await;
                tokio::time::sleep(Duration::from_secs(45)).await;
            }
        });
    }

    /// The last-known IPv4 for `mac`, if any (case-insensitive).
    pub async fn ip_for_mac(&self, mac: &str) -> Option<String> {
        self.cache.lock().await.get(&mac.to_lowercase()).cloned()
    }

    async fn refresh(&self) {
        if let Some(prefix) = bridge_subnet(&self.bridge).await {
            sweep(&prefix).await;
        }
        let map = read_neighbors(&self.bridge).await;
        let count = map.len();
        *self.cache.lock().await = map;
        debug!(bridge = %self.bridge, macs = count, "ip discovery refreshed");
    }
}

/// The first three octets of the bridge's IPv4 (`172.16.56.82/24` -> `172.16.56`).
/// Only /24 sweeps are supported (the MVP lab network, section 18).
async fn bridge_subnet(bridge: &str) -> Option<String> {
    let out = Command::new("ip")
        .args(["-4", "-o", "addr", "show", "dev", bridge])
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // "... inet 172.16.56.82/24 ..."
    let addr = text
        .split_whitespace()
        .skip_while(|t| *t != "inet")
        .nth(1)?;
    let ip = addr.split('/').next()?;
    let mut octets: Vec<&str> = ip.split('.').collect();
    if octets.len() != 4 {
        return None;
    }
    octets.pop();
    Some(octets.join("."))
}

/// Ping every host in `<prefix>.1..=254` (bounded concurrency) so the kernel
/// resolves and caches their ARP entries. Failures are ignored.
async fn sweep(prefix: &str) {
    let sem = Arc::new(Semaphore::new(64));
    let mut handles = Vec::with_capacity(254);
    for i in 1..=254u32 {
        let permit = sem.clone().acquire_owned().await;
        let target = format!("{prefix}.{i}");
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let _ = Command::new("ping")
                .args(["-c", "1", "-W", "1", &target])
                .output()
                .await;
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

/// Parse `ip -4 neigh show dev <bridge>` into a MAC(lowercase) -> IPv4 map,
/// preferring reachable entries and skipping ones with no usable address.
async fn read_neighbors(bridge: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(out) = Command::new("ip")
        .args(["-4", "neigh", "show", "dev", bridge])
        .output()
        .await
    else {
        return map;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        // "172.16.56.31 lladdr 02:2b:ec:c8:e3:fe REACHABLE"
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(ip), Some(mac_pos)) = (f.first(), f.iter().position(|t| *t == "lladdr")) else {
            continue;
        };
        let Some(mac) = f.get(mac_pos + 1) else {
            continue;
        };
        let state = f.last().copied().unwrap_or("");
        if state == "FAILED" || state == "INCOMPLETE" {
            continue;
        }
        let mac = mac.to_lowercase();
        // Prefer a REACHABLE entry over a STALE one for the same MAC.
        if state == "REACHABLE" || !map.contains_key(&mac) {
            map.insert(mac, ip.to_string());
        }
    }
    map
}
