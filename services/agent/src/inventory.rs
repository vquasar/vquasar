//! Host inventory collection (design document, section 9).
//!
//! Milestone 0 gathers only the fields the first release needs — hostname,
//! architecture, kernel, logical CPUs, and memory (section 40). The richer
//! inventory (NUMA, PCI/IOMMU, GPUs, SEV-SNP/TDX) is left as `None` on the
//! model so it can be filled in later without a schema change.
//!
//! Everything here is read from `/proc` and `std`, so it needs no extra
//! dependencies.

use std::fs;

use ch_model::{HostState, HostStatus};

/// Collect the current host inventory into a [`HostStatus`].
///
/// The agent process being up is what makes the host `Ready`; heartbeat-driven
/// transitions to `NotReady` are the control plane's job (section 26).
pub fn collect() -> HostStatus {
    HostStatus {
        state: HostState::Ready,
        hostname: hostname(),
        architecture: Some(std::env::consts::ARCH.to_string()),
        kernel_version: read_trimmed("/proc/sys/kernel/osrelease"),
        cloud_hypervisor_version: None, // populated in Milestone 2
        logical_cpus: logical_cpus(),
        cpu_model: cpu_model(),
        total_memory_bytes: meminfo_bytes("MemTotal"),
        available_memory_bytes: meminfo_bytes("MemAvailable"),
        vm_count: 0,
        last_heartbeat: None,
    }
}

fn hostname() -> Option<String> {
    read_trimmed("/proc/sys/kernel/hostname")
}

fn logical_cpus() -> Option<u32> {
    std::thread::available_parallelism()
        .ok()
        .map(|n| n.get() as u32)
}

fn cpu_model() -> Option<String> {
    let contents = fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim() == "model name" {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

/// Parse a `/proc/meminfo` field (reported in kB) into bytes.
fn meminfo_bytes(field: &str) -> Option<u64> {
    let contents = fs::read_to_string("/proc/meminfo").ok()?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            // Format: "MemTotal:       16384000 kB"
            let kb: u64 = rest
                .trim_start_matches(':')
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_reports_architecture_and_ready_state() {
        let status = collect();
        assert_eq!(status.state, HostState::Ready);
        assert!(status.architecture.is_some());
    }

    #[test]
    fn on_linux_memory_and_cpus_are_present() {
        // The primary target is Linux x86_64 (section 39); guard so the suite
        // still passes on other developer platforms.
        if cfg!(target_os = "linux") {
            let status = collect();
            assert!(status.logical_cpus.unwrap_or(0) >= 1);
            assert!(status.total_memory_bytes.unwrap_or(0) > 0);
        }
    }
}
