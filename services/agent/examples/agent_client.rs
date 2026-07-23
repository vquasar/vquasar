//! Milestone 2 gRPC client harness (design document, section 12).
//!
//! Drives a running `ch-agent` over its `HostAgent` gRPC API, so the agent's
//! VM lifecycle can be exercised exactly as the control plane will. Used both
//! interactively and by the restart-survival scenario in
//! `scripts/agent-restart-demo.sh`.

use std::path::PathBuf;

use ch_model::{
    BootSpec, CpuSpec, DesiredPowerState, DiskSpec, MemorySpec, PlacementSpec, VirtualMachineSpec,
};
use ch_proto::agent::host_agent_client::HostAgentClient;
use ch_proto::agent::{
    DeleteVmRequest, EnsureVmRequest, GetHostInfoRequest, GetVmRequest, ListVmsRequest,
    StartVmRequest, StopVmRequest,
};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "agent_client", about = "Drive a ch-agent over gRPC")]
struct Cli {
    /// Agent gRPC endpoint.
    #[arg(long, default_value = "http://127.0.0.1:9500")]
    endpoint: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Fetch host inventory.
    HostInfo,
    /// List all VMs.
    List,
    /// Get one VM's observed state.
    Get {
        vm_id: String,
    },
    /// Create + reconcile a VM (direct-kernel or firmware boot).
    Ensure(EnsureArgs),
    Start {
        vm_id: String,
    },
    Stop {
        vm_id: String,
    },
    Delete {
        vm_id: String,
    },
}

#[derive(Debug, Parser)]
struct EnsureArgs {
    #[arg(long)]
    vm_id: String,
    #[arg(long)]
    name: String,
    #[arg(long)]
    kernel: Option<PathBuf>,
    #[arg(long)]
    firmware: Option<PathBuf>,
    #[arg(long)]
    initramfs: Option<PathBuf>,
    #[arg(long, default_value = "root=/dev/vda1 rw console=ttyS0")]
    cmdline: String,
    #[arg(long = "disk")]
    disks: Vec<PathBuf>,
    #[arg(long = "readonly-disk")]
    readonly_disks: Vec<PathBuf>,
    #[arg(long, default_value_t = 2)]
    cpus: u32,
    #[arg(long, default_value_t = 2048)]
    memory_mib: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut client = HostAgentClient::connect(cli.endpoint).await?;

    match cli.command {
        Command::HostInfo => {
            let resp = client
                .get_host_info(GetHostInfoRequest {})
                .await?
                .into_inner();
            println!("{resp:#?}");
        }
        Command::List => {
            let resp = client.list_vms(ListVmsRequest {}).await?.into_inner();
            println!("{resp:#?}");
        }
        Command::Get { vm_id } => {
            let resp = client.get_vm(GetVmRequest { vm_id }).await?.into_inner();
            println!("{resp:#?}");
        }
        Command::Ensure(args) => {
            let spec = build_spec(&args)?;
            let resp = client
                .ensure_vm(EnsureVmRequest {
                    vm_id: args.vm_id,
                    name: args.name,
                    spec_json: serde_json::to_vec(&spec)?,
                })
                .await?
                .into_inner();
            println!("{resp:#?}");
        }
        Command::Start { vm_id } => {
            let resp = client
                .start_vm(StartVmRequest { vm_id })
                .await?
                .into_inner();
            println!("{resp:#?}");
        }
        Command::Stop { vm_id } => {
            let resp = client.stop_vm(StopVmRequest { vm_id }).await?.into_inner();
            println!("{resp:#?}");
        }
        Command::Delete { vm_id } => {
            let resp = client
                .delete_vm(DeleteVmRequest { vm_id })
                .await?
                .into_inner();
            println!("{resp:#?}");
        }
    }
    Ok(())
}

fn build_spec(args: &EnsureArgs) -> anyhow::Result<VirtualMachineSpec> {
    let boot = match (&args.firmware, &args.kernel) {
        (Some(firmware), _) => BootSpec::Firmware {
            firmware: firmware.clone(),
        },
        (None, Some(kernel)) => BootSpec::DirectKernel {
            kernel: kernel.clone(),
            initramfs: args.initramfs.clone(),
            cmdline: Some(args.cmdline.clone()),
        },
        (None, None) => anyhow::bail!("provide either --kernel or --firmware"),
    };

    let mut disks: Vec<DiskSpec> = args.disks.iter().map(DiskSpec::raw).collect();
    for ro in &args.readonly_disks {
        disks.push(DiskSpec {
            path: ro.clone(),
            readonly: true,
            image_type: ch_model::DiskImageType::Raw,
        });
    }

    Ok(VirtualMachineSpec {
        desired_power_state: DesiredPowerState::Running,
        cpu: CpuSpec {
            boot_vcpus: args.cpus,
            max_vcpus: args.cpus,
        },
        memory: MemorySpec {
            size_mib: args.memory_mib,
        },
        boot,
        disks,
        network_interfaces: vec![],
        placement: PlacementSpec::default(),
    })
}
