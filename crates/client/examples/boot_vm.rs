//! Milestone 1 end-to-end harness (design document, sections 10 and 42).
//!
//! Launches a real `cloud-hypervisor`, creates and boots a VM from a
//! [`VirtualMachineSpec`] through the [`Hypervisor`] trait, and tails the guest
//! serial console until a marker string appears (or a timeout elapses). This is
//! the "a Linux VM boots successfully and produces serial output" acceptance
//! test for Milestone 1, exercised through the same code path the host agent
//! will use.
//!
//! Example (direct-kernel boot of an Ubuntu cloud image):
//!
//! ```text
//! cargo run -p vquasar-client --example boot_vm -- \
//!   --binary      /var/lib/vquasar/bin/cloud-hypervisor \
//!   --kernel      /var/lib/vquasar/images/vmlinuz-7.0.0-28-generic \
//!   --initramfs   /var/lib/vquasar/images/initrd.img-7.0.0-28-generic \
//!   --cmdline     "root=/dev/vda1 rw console=ttyS0 systemd.mask=systemd-networkd-wait-online.service" \
//!   --disk        /var/lib/vquasar/volumes/dk01.raw \
//!   --readonly-disk /var/lib/vquasar/seed/seed.iso \
//!   --runtime-dir /var/lib/vquasar/vms/dk01 \
//!   --marker      "VQUASAR-BOOT-OK"
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use tracing::{error, info};

use vquasar_client::config::TranslateOptions;
use vquasar_client::{CloudHypervisor, Hypervisor, LaunchConfig, ProcessConfig, SerialTarget};
use vquasar_model::{
    BootSpec, CpuSpec, DesiredPowerState, DiskSpec, MemorySpec, PlacementSpec, VirtualMachineSpec,
};

#[derive(Debug, Parser)]
#[command(
    name = "boot_vm",
    about = "Boot a VM on real Cloud Hypervisor via the Hypervisor trait"
)]
struct Cli {
    /// Path to the cloud-hypervisor binary.
    #[arg(long)]
    binary: PathBuf,
    /// Direct-kernel image (bzImage or PVH vmlinux). Mutually exclusive with
    /// --firmware.
    #[arg(
        long,
        required_unless_present = "firmware",
        conflicts_with = "firmware"
    )]
    kernel: Option<PathBuf>,
    /// Architectural firmware (EDK2 CLOUDHV.fd) to boot the disk's own
    /// bootloader (modern UEFI). Mutually exclusive with --kernel.
    #[arg(long)]
    firmware: Option<PathBuf>,
    /// Optional initramfs (direct-kernel boot only).
    #[arg(long)]
    initramfs: Option<PathBuf>,
    /// Kernel command line.
    #[arg(long, default_value = "root=/dev/vda1 rw console=ttyS0")]
    cmdline: String,
    /// Writable raw disk(s), attached in order (first becomes /dev/vda).
    #[arg(long = "disk")]
    disks: Vec<PathBuf>,
    /// Read-only raw disk(s) (e.g. a cloud-init seed ISO).
    #[arg(long = "readonly-disk")]
    readonly_disks: Vec<PathBuf>,
    /// Boot vCPUs.
    #[arg(long, default_value_t = 2)]
    cpus: u32,
    /// Memory in MiB.
    #[arg(long, default_value_t = 2048)]
    memory_mib: u64,
    /// Per-VM runtime directory (holds api.sock, serial.log, ch.log).
    #[arg(long)]
    runtime_dir: PathBuf,
    /// Seconds to wait for the marker before giving up.
    #[arg(long, default_value_t = 180)]
    wait_secs: u64,
    /// Serial marker string that signals a successful boot.
    #[arg(long, default_value = "VQUASAR-BOOT-OK")]
    marker: String,
    /// Leave the VM running on success instead of shutting it down.
    #[arg(long)]
    keep_running: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.runtime_dir)?;
    let api_socket = cli.runtime_dir.join("api.sock");
    let serial_log = cli.runtime_dir.join("serial.log");
    let ch_log = cli.runtime_dir.join("ch.log");
    // A stale socket from a previous run would make CH fail to bind.
    let _ = std::fs::remove_file(&api_socket);
    let _ = std::fs::remove_file(&serial_log);

    // Build the desired-state spec.
    let mut disks: Vec<DiskSpec> = cli.disks.iter().map(DiskSpec::raw).collect();
    for ro in &cli.readonly_disks {
        disks.push(DiskSpec {
            path: ro.clone(),
            readonly: true,
            image_type: vquasar_model::DiskImageType::Raw,
            source: None,
            size_bytes: None,
            pool: None,
            policy: None,
            pinned_host: None,
        });
    }

    let spec = VirtualMachineSpec {
        desired_power_state: DesiredPowerState::Running,
        cpu: CpuSpec {
            boot_vcpus: cli.cpus,
            max_vcpus: cli.cpus,
        },
        memory: MemorySpec {
            size_mib: cli.memory_mib,
            max_size_mib: None,
        },
        boot: match (&cli.firmware, &cli.kernel) {
            (Some(firmware), _) => BootSpec::Firmware {
                firmware: firmware.clone(),
            },
            (None, Some(kernel)) => BootSpec::DirectKernel {
                kernel: kernel.clone(),
                initramfs: cli.initramfs.clone(),
                cmdline: Some(cli.cmdline.clone()),
            },
            (None, None) => unreachable!("clap requires --kernel or --firmware"),
        },
        disks,
        network_interfaces: vec![],
        placement: PlacementSpec::default(),
        cloud_init: None,
        machine_type: vquasar_model::MachineType::Standard,
    };
    spec.validate()?;

    // Launch the VMM and drive it through the trait.
    let launch = LaunchConfig::new(
        ProcessConfig {
            binary: cli.binary.clone(),
            api_socket: api_socket.clone(),
            log_file: Some(ch_log.clone()),
            extra_args: vec![],
        },
        TranslateOptions {
            serial: SerialTarget::File(serial_log.to_string_lossy().into_owned()),
            taps: vec![],
        },
    );

    info!(binary = %cli.binary.display(), "launching cloud-hypervisor");
    let mut hv = CloudHypervisor::launch(launch).await?;
    info!(pid = ?hv.pid(), "VMM ready; creating VM");

    // Once the VMM is launched we own its lifecycle: any failure below must
    // terminate it, otherwise the process leaks (drop intentionally does NOT
    // kill, so that VMs survive agent restarts — design section 11).
    let outcome = run_and_wait(&hv, &spec, &serial_log, &cli).await;

    match outcome {
        Ok(true) => {
            info!(marker = %cli.marker, serial = %serial_log.display(), "BOOT OK — marker observed on serial console");
            if cli.keep_running {
                info!("leaving VM running (--keep-running)");
            } else {
                info!("shutting down VM");
                let _ = hv.shutdown().await;
                hv.kill().await?;
            }
            Ok(())
        }
        Ok(false) => {
            error!(serial = %serial_log.display(), "marker not seen within timeout");
            let _ = hv.shutdown().await;
            let _ = hv.kill().await;
            anyhow::bail!(
                "boot marker '{}' not observed within {}s",
                cli.marker,
                cli.wait_secs
            );
        }
        Err(e) => {
            error!(error = %e, "boot failed; terminating VMM");
            let _ = hv.kill().await;
            Err(e)
        }
    }
}

/// Create + boot the VM and wait for the serial marker. Returns whether the
/// marker was seen. Any error leaves cleanup to the caller.
async fn run_and_wait(
    hv: &CloudHypervisor,
    spec: &VirtualMachineSpec,
    serial_log: &std::path::Path,
    cli: &Cli,
) -> anyhow::Result<bool> {
    hv.create(spec).await?;
    info!("VM created; booting");
    hv.boot().await?;
    info!(state = ?hv.info().await?.state, "VM booted; waiting for serial marker");
    Ok(wait_for_marker(serial_log, &cli.marker, Duration::from_secs(cli.wait_secs)).await)
}

/// Poll the serial log file until it contains `marker` or `timeout` elapses.
async fn wait_for_marker(serial_log: &std::path::Path, marker: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(contents) = tokio::fs::read(serial_log).await {
            if String::from_utf8_lossy(&contents).contains(marker) {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
