// Types mirroring the ch-control REST API (design sections 6, 14).

export type DesiredPowerState = "Running" | "Stopped";

export interface CpuSpec {
  boot_vcpus: number;
  max_vcpus: number;
}

export interface MemorySpec {
  size_mib: number;
}

export type BootSpec =
  | { type: "direct_kernel"; kernel: string; initramfs?: string | null; cmdline?: string | null }
  | { type: "firmware"; firmware: string };

export interface DiskSpec {
  path: string;
  readonly?: boolean;
  image_type?: "raw" | "qcow2";
}

export interface NetworkInterfaceSpec {
  network_id: string;
  mac?: string | null;
}

export interface PlacementSpec {
  host?: string | null;
}

export interface VirtualMachineSpec {
  desired_power_state: DesiredPowerState;
  cpu: CpuSpec;
  memory: MemorySpec;
  boot: BootSpec;
  disks: DiskSpec[];
  network_interfaces: NetworkInterfaceSpec[];
  placement: PlacementSpec;
}

export type VmPhase =
  | "Pending"
  | "Scheduling"
  | "Creating"
  | "Stopped"
  | "Starting"
  | "Running"
  | "Stopping"
  | "Migrating"
  | "Failed"
  | "Deleting";

export interface Vm {
  id: string;
  name: string;
  spec: VirtualMachineSpec;
  phase: VmPhase;
  host_id: string | null;
  observed_generation: number;
  message: string | null;
  ip_address: string | null;
  created_at: string;
  updated_at: string;
  generation: number;
}

export type HostState = "Ready" | "NotReady" | "Maintenance" | "Disabled";

export interface Host {
  id: string;
  name: string;
  endpoint: string;
  schedulable: boolean;
  state: HostState;
  hostname: string | null;
  architecture: string | null;
  kernel_version: string | null;
  cloud_hypervisor_version: string | null;
  logical_cpus: number | null;
  cpu_model: string | null;
  total_memory_bytes: number | null;
  available_memory_bytes: number | null;
  vm_count: number;
  last_heartbeat: string | null;
  created_at: string;
  updated_at: string;
  generation: number;
}

export interface Network {
  id: string;
  name: string;
  vlan: number | null;
  created_at: string;
  updated_at: string;
}

export interface Task {
  id: string;
  task_type: string;
  state: "Pending" | "Running" | "Succeeded" | "Failed" | "Cancelled";
  progress: number;
  vm_id: string | null;
  message: string | null;
  created_at: string;
  updated_at: string;
}

export interface Event {
  id: string;
  ts: string;
  resource_type: string;
  resource_id: string | null;
  event_type: string;
  severity: string;
  message: string;
  metadata: unknown;
}

export interface CreateVmRequest {
  name: string;
  spec: VirtualMachineSpec;
}

export interface Accepted {
  vm_id: string;
  task_id: string;
}
