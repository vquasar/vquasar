// Types mirroring the ch-control REST API (design sections 6, 14).

export type DesiredPowerState = "Running" | "Stopped";

export interface CpuSpec {
  boot_vcpus: number;
  max_vcpus: number;
}

export interface MemorySpec {
  size_mib: number;
  max_size_mib?: number | null;
}

export type BootSpec =
  | { type: "direct_kernel"; kernel: string; initramfs?: string | null; cmdline?: string | null }
  | { type: "firmware"; firmware: string };

export interface DiskSpec {
  path: string;
  readonly?: boolean;
  image_type?: "raw" | "qcow2";
  source?: string | null;
  size_bytes?: number | null;
}

export interface CloudInitSpec {
  hostname?: string | null;
  ssh_authorized_keys?: string[];
  password?: string | null;
  user_data?: string | null;
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
  cloud_init?: CloudInitSpec | null;
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
  // IPAM (M13a): a family is control-plane-managed (static) when its cidr is set.
  cidr_v4: string | null;
  gateway_v4: string | null;
  cidr_v6: string | null;
  gateway_v6: string | null;
  dns: string[];
  pool_v4_start: string | null;
  pool_v4_end: string | null;
  pool_v6_start: string | null;
  pool_v6_end: string | null;
  created_at: string;
  updated_at: string;
}

export interface IpAllocation {
  id: string;
  network_id: string;
  ip: string;
  family: number;
  vm_id: string | null;
  nic_index: number;
  mac: string;
  created_at: string;
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

// ---- images & templates (design M9) --------------------------------------

export interface Image {
  id: string;
  name: string;
  source_path: string;
  format: "raw" | "qcow2";
  boot: BootSpec;
  default_size_bytes: number | null;
  cloud_init: boolean;
  os: string | null;
  created_at: string;
  updated_at: string;
}

export interface Template {
  id: string;
  name: string;
  image_id: string;
  boot_vcpus: number;
  max_vcpus: number;
  memory_mib: number;
  disk_size_bytes: number | null;
  disk_format: "raw" | "qcow2";
  network_id: string | null;
  cloud_init: CloudInitSpec | null;
  created_at: string;
  updated_at: string;
}

export interface CreateNetworkRequest {
  name: string;
  vlan?: number | null;
  // IPAM (M13a); omit a family's cidr to leave it on DHCP.
  cidr_v4?: string | null;
  gateway_v4?: string | null;
  cidr_v6?: string | null;
  gateway_v6?: string | null;
  dns?: string[];
  pool_v4_start?: string | null;
  pool_v4_end?: string | null;
  pool_v6_start?: string | null;
  pool_v6_end?: string | null;
}

export interface CreateImageRequest {
  name: string;
  source_path: string;
  format: "raw" | "qcow2";
  boot: BootSpec;
  default_size_bytes?: number | null;
  cloud_init?: boolean;
  os?: string | null;
}

export interface CreateTemplateRequest {
  name: string;
  image_id: string;
  boot_vcpus: number;
  max_vcpus: number;
  memory_mib: number;
  disk_size_bytes?: number | null;
  disk_format?: "raw" | "qcow2";
  network_id?: string | null;
  cloud_init?: CloudInitSpec | null;
}

export interface TemplateOverrides {
  boot_vcpus?: number;
  max_vcpus?: number;
  memory_mib?: number;
  memory_max_mib?: number;
  disk_size_bytes?: number;
  network_id?: string;
  cloud_init?: CloudInitSpec;
}

export interface CreateVmFromTemplateRequest {
  name: string;
  template_id: string;
  overrides?: TemplateOverrides;
}

export interface UpdateVmRequest {
  name?: string;
  boot_vcpus?: number;
  max_vcpus?: number;
  memory_mib?: number;
  memory_max_mib?: number;
  grow_disk?: { index: number; size_bytes: number };
  add_disk?: { size_bytes: number; image_type?: "raw" | "qcow2" };
  add_nic?: { network_id: string };
}

// --- IAM (design M12b) ---

export interface AuthConfigView {
  enabled: boolean;
  issuer: string;
  client_id: string;
}

export interface Me {
  authenticated: boolean;
  username?: string | null;
  email?: string | null;
  permissions: string[];
}

export interface Role {
  id: string;
  name: string;
  description?: string | null;
  builtin: boolean;
}

export interface RoleView extends Role {
  permissions: string[];
}

export interface User {
  id: string;
  subject: string;
  username: string;
  email?: string | null;
  display_name?: string | null;
}

export interface UserView extends User {
  roles: Role[];
}

export interface GroupRoleView {
  group: string;
  role: string;
}

export interface CreateRoleRequest {
  name: string;
  description?: string | null;
  permissions: string[];
}

export interface UpdateRoleRequest {
  description?: string | null;
  permissions: string[];
}
