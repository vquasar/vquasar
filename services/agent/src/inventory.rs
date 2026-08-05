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

use vquasar_model::{HostState, HostStatus};

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
        cpu_vendor: cpuinfo_field("vendor_id"),
        cpu_features: guest_cpu_features(),
        total_memory_bytes: meminfo_bytes("MemTotal"),
        available_memory_bytes: meminfo_bytes("MemAvailable"),
        vm_count: 0,
        last_heartbeat: None,
    }
}

/// Probe the Cloud Hypervisor version by running `<binary> --version`.
///
/// Returns e.g. `"v53.0"`. Run once at startup rather than per request.
pub fn cloud_hypervisor_version(binary: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // First line looks like: "cloud-hypervisor v53.0".
    text.lines()
        .next()
        .and_then(|line| line.split_whitespace().last())
        .map(|s| s.to_string())
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
    cpuinfo_field("model name")
}

/// Return the value of the first matching `/proc/cpuinfo` field (e.g.
/// `model name`, `vendor_id`). All physical cores repeat the same values, so
/// the first hit is representative.
fn cpuinfo_field(field: &str) -> Option<String> {
    let contents = fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim() == field {
                let v = value.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Curated allow-list of guest-visible x86 CPU **ISA** feature flags, as they
/// appear in `/proc/cpuinfo`. We report only these for migration compatibility
/// and deliberately exclude host/topology/mitigation/hypervisor flags (`ht`,
/// `pti`, `ibrs_enhanced`, `constant_tsc`, `vmx`, `hypervisor`, …) which differ
/// between otherwise-identical hosts and would cause spurious incompatibility.
/// A missing entry only under-reports (a rare feature goes ungated); the set is
/// kept broad enough to catch the ISA gaps that actually fault guests
/// (AVX-512 variants, VNNI, MPX, UMIP, SHA, …).
const GUEST_CPU_FEATURES: &[&str] = &[
    // Base ISA / legacy SIMD
    "fpu",
    "mmx",
    "mmxext",
    "fxsr",
    "sse",
    "sse2",
    "sse3",
    "pni",
    "ssse3",
    "sse4_1",
    "sse4_2",
    "sse4a",
    "3dnow",
    "3dnowext",
    "3dnowprefetch",
    "cx8",
    "cx16",
    "cmov",
    "clflush",
    "movbe",
    // 64-bit / paging / misc base
    "lm",
    "nx",
    "syscall",
    "rdtscp",
    "pdpe1gb",
    "pcid",
    "invpcid",
    "fsgsbase",
    "rdpid",
    // AVX / AVX2
    "avx",
    "avx2",
    "f16c",
    "fma",
    "fma4",
    // AVX-512
    "avx512f",
    "avx512dq",
    "avx512cd",
    "avx512bw",
    "avx512vl",
    "avx512ifma",
    "avx512vbmi",
    "avx512_vbmi2",
    "avx512_vnni",
    "avx512_bitalg",
    "avx512_vpopcntdq",
    "avx512_4vnniw",
    "avx512_4fmaps",
    "avx512_bf16",
    "avx512_fp16",
    "avx512er",
    "avx512pf",
    // AVX-VNNI (non-512) and newer
    "avx_vnni",
    "avx_vnni_int8",
    "avx_ne_convert",
    // Bit manipulation / integer
    "bmi1",
    "bmi2",
    "abm",
    "lzcnt",
    "popcnt",
    "adx",
    "tbm",
    // Crypto
    "aes",
    "pclmulqdq",
    "sha_ni",
    "vaes",
    "vpclmulqdq",
    "gfni",
    // Random
    "rdrand",
    "rdseed",
    // Memory protection
    "mpx",
    "umip",
    "pku",
    "smap",
    "smep",
    // XSAVE family
    "xsave",
    "xsaveopt",
    "xsavec",
    "xsaves",
    "xgetbv1",
    // TSX
    "rtm",
    "hle",
    // Cacheline control
    "clflushopt",
    "clwb",
    "cldemote",
    "clzero",
    // Direct stores / serialization / newer misc
    "movdiri",
    "movdir64b",
    "serialize",
    "waitpkg",
    "enqcmd",
    "ptwrite",
    "pconfig",
    "wbnoinvd",
    "prefetchwt1",
    "erms",
    // AMX
    "amx_tile",
    "amx_int8",
    "amx_bf16",
];

/// The subset of this host's `/proc/cpuinfo` flags that are guest-visible ISA
/// features (intersection with [`GUEST_CPU_FEATURES`]), sorted for stable
/// comparison and display.
fn guest_cpu_features() -> Vec<String> {
    let Some(flags) = cpuinfo_field("flags") else {
        return Vec::new();
    };
    let present: std::collections::HashSet<&str> = flags.split_whitespace().collect();
    let mut out: Vec<String> = GUEST_CPU_FEATURES
        .iter()
        .filter(|f| present.contains(**f))
        .map(|f| f.to_string())
        .collect();
    out.sort();
    out
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
