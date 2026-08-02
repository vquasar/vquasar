//! Milestone 1 integration test: boot a real VM on Cloud Hypervisor and verify
//! it produces serial output (design document, sections 38 and 42).
//!
//! This test is **opt-in**: it only runs when `/dev/kvm` exists and the asset
//! paths are provided via environment variables. Otherwise it prints a skip
//! notice and passes, so `cargo test --workspace` stays green on machines
//! without KVM or the Ubuntu image assets.
//!
//! To run it (e.g. on the `dome` lab host):
//!
//! ```text
//! export CH_IT_BINARY=/var/lib/ch-orchestrator/bin/cloud-hypervisor
//! export CH_IT_KERNEL=/var/lib/ch-orchestrator/images/vmlinuz-7.0.0-28-generic
//! export CH_IT_INITRAMFS=/var/lib/ch-orchestrator/images/initrd.img-7.0.0-28-generic
//! export CH_IT_ROOTFS=/var/lib/ch-orchestrator/images/ubuntu-26.04.raw
//! cargo test -p ch-client --test boot_integration -- --nocapture
//! ```
//!
//! The root disk is attached **read-only** and we only wait for the kernel
//! banner, so the test is fast and never mutates the shared base image.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ch_client::config::TranslateOptions;
use ch_client::{CloudHypervisor, Hypervisor, LaunchConfig, ProcessConfig, SerialTarget};
use ch_model::{
    BootSpec, CpuSpec, DesiredPowerState, DiskImageType, DiskSpec, MemorySpec, PlacementSpec,
    VirtualMachineSpec,
};

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).map(PathBuf::from)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boots_real_vm_and_emits_serial_output() {
    if !PathBuf::from("/dev/kvm").exists() {
        eprintln!("SKIP: /dev/kvm not present");
        return;
    }
    let (Some(binary), Some(kernel), Some(rootfs)) = (
        env_path("CH_IT_BINARY"),
        env_path("CH_IT_KERNEL"),
        env_path("CH_IT_ROOTFS"),
    ) else {
        eprintln!("SKIP: set CH_IT_BINARY, CH_IT_KERNEL, CH_IT_ROOTFS to run this test");
        return;
    };
    let initramfs = env_path("CH_IT_INITRAMFS");

    // Isolated runtime directory for this test's VM.
    let runtime_dir = std::env::temp_dir().join(format!("ch-it-{}", std::process::id()));
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let api_socket = runtime_dir.join("api.sock");
    let serial_log = runtime_dir.join("serial.log");
    let _ = std::fs::remove_file(&api_socket);

    let spec = VirtualMachineSpec {
        desired_power_state: DesiredPowerState::Running,
        cpu: CpuSpec {
            boot_vcpus: 1,
            max_vcpus: 1,
        },
        memory: MemorySpec {
            size_mib: 1024,
            max_size_mib: None,
        },
        boot: BootSpec::DirectKernel {
            kernel,
            initramfs,
            // Read-only root; we only need the kernel to start and log.
            cmdline: Some("console=ttyS0 root=/dev/vda1 ro".to_string()),
        },
        // Base image attached read-only so the test never mutates it.
        disks: vec![DiskSpec {
            path: rootfs,
            readonly: true,
            image_type: DiskImageType::Raw,
            source: None,
            size_bytes: None,
        }],
        network_interfaces: vec![],
        placement: PlacementSpec::default(),
        cloud_init: None,
    };
    spec.validate().expect("spec valid");

    let launch = LaunchConfig::new(
        ProcessConfig {
            binary,
            api_socket,
            log_file: Some(runtime_dir.join("ch.log")),
            extra_args: vec![],
        },
        TranslateOptions {
            serial: SerialTarget::File(serial_log.to_string_lossy().into_owned()),
            taps: vec![],
        },
    );

    let mut hv = CloudHypervisor::launch(launch).await.expect("launch VMM");
    let result = drive(&hv, &spec, &serial_log).await;

    // Always clean up the VMM (drop does not kill, by design).
    let _ = hv.shutdown().await;
    hv.kill().await.expect("kill VMM");
    let _ = std::fs::remove_dir_all(&runtime_dir);

    let seen = result.expect("boot sequence");
    assert!(seen, "expected the kernel banner on the serial console");
}

async fn drive(
    hv: &CloudHypervisor,
    spec: &VirtualMachineSpec,
    serial_log: &std::path::Path,
) -> ch_client::Result<bool> {
    hv.create(spec).await?;
    hv.boot().await?;
    assert_eq!(
        hv.info().await?.state,
        ch_client::HypervisorState::Running,
        "VM should report Running after boot"
    );

    // Wait up to 45s for the kernel banner to appear on serial.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Ok(bytes) = std::fs::read(serial_log) {
            if String::from_utf8_lossy(&bytes).contains("Linux version") {
                return Ok(true);
            }
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
