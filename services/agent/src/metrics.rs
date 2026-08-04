//! Live per-VM resource metrics (design M15a).
//!
//! CPU and memory are observed host-side from the VMM process (its vCPU threads
//! run inside it, so its CPU time ≈ the guest's), sampled over a short window
//! for an instantaneous percentage. Disk/network are cumulative counters read
//! from Cloud Hypervisor and aggregated across devices.

use std::time::Duration;

/// A point-in-time metrics sample for one VM.
#[derive(Debug, Clone, Default)]
pub struct VmMetrics {
    pub running: bool,
    pub cpu_pct: f64,
    pub mem_bytes: u64,
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    pub disk_read_ops: u64,
    pub disk_write_ops: u64,
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
    pub net_rx_packets: u64,
    pub net_tx_packets: u64,
}

/// USER_HZ on Linux (jiffies per second); 100 on all common configs.
const CLK_TCK: f64 = 100.0;
const CPU_SAMPLE: Duration = Duration::from_millis(200);

/// Sum of the process's utime+stime in jiffies from /proc/<pid>/stat.
fn proc_jiffies(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 2 (comm) is parenthesised and may contain spaces; fields resume
    // after the last ')'. utime/stime are fields 14/15 overall.
    let after = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = after.split_whitespace().collect();
    // Index 0 here is field 3 (state); utime=field14=index 11, stime=index 12.
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// Resident set size in bytes from /proc/<pid>/status.
fn proc_rss_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// Sum a CH counter across all devices by counter-key name (ids vary).
fn sum_counter(counters: &serde_json::Value, keys: &[&str]) -> u64 {
    let Some(map) = counters.as_object() else {
        return 0;
    };
    let mut total = 0u64;
    for dev in map.values() {
        if let Some(dev) = dev.as_object() {
            for k in keys {
                if let Some(v) = dev.get(*k).and_then(|v| v.as_u64()) {
                    total += v;
                }
            }
        }
    }
    total
}

/// Sample live metrics for a VMM `pid` plus its CH `counters` map.
pub async fn sample(pid: u32, counters: &serde_json::Value) -> VmMetrics {
    // CPU: two jiffie reads a short window apart -> instantaneous percent.
    let cpu_pct = match proc_jiffies(pid) {
        Some(j0) => {
            tokio::time::sleep(CPU_SAMPLE).await;
            match proc_jiffies(pid) {
                Some(j1) => {
                    let dj = j1.saturating_sub(j0) as f64;
                    (dj / CLK_TCK) / CPU_SAMPLE.as_secs_f64() * 100.0
                }
                None => 0.0,
            }
        }
        None => 0.0,
    };

    VmMetrics {
        running: true,
        cpu_pct,
        mem_bytes: proc_rss_bytes(pid).unwrap_or(0),
        // CH block counters (names differ slightly across versions).
        disk_read_bytes: sum_counter(counters, &["read_bytes"]),
        disk_write_bytes: sum_counter(counters, &["write_bytes"]),
        disk_read_ops: sum_counter(counters, &["read_ops", "read_operations"]),
        disk_write_ops: sum_counter(counters, &["write_ops", "write_operations"]),
        net_rx_bytes: sum_counter(counters, &["rx_bytes"]),
        net_tx_bytes: sum_counter(counters, &["tx_bytes"]),
        net_rx_packets: sum_counter(counters, &["rx_frames", "rx_packets"]),
        net_tx_packets: sum_counter(counters, &["tx_frames", "tx_packets"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_counters_across_devices() {
        let c = serde_json::json!({
            "_disk0": {"read_bytes": 100, "write_bytes": 50, "read_ops": 4, "write_ops": 2},
            "_disk1": {"read_bytes": 25,  "write_bytes": 5,  "read_ops": 1, "write_ops": 1},
            "_net0":  {"rx_bytes": 1000, "tx_bytes": 200, "rx_frames": 10, "tx_frames": 3}
        });
        assert_eq!(sum_counter(&c, &["read_bytes"]), 125);
        assert_eq!(sum_counter(&c, &["write_ops", "write_operations"]), 3);
        assert_eq!(sum_counter(&c, &["rx_bytes"]), 1000);
        assert_eq!(sum_counter(&c, &["tx_frames", "tx_packets"]), 3);
        assert_eq!(sum_counter(&serde_json::json!({}), &["read_bytes"]), 0);
    }

    #[test]
    fn own_process_has_cpu_and_rss() {
        let pid = std::process::id();
        assert!(proc_jiffies(pid).is_some());
        assert!(proc_rss_bytes(pid).unwrap_or(0) > 0);
    }
}
