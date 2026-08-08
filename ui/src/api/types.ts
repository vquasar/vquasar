// Types mirroring the vquasar-control REST API (design sections 6, 14).

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
  addresses?: string[];
  security_groups?: string[];
}

export interface PlacementSpec {
  host?: string | null;
}

export type MachineType = "standard" | "microvm";

export interface VirtualMachineSpec {
  desired_power_state: DesiredPowerState;
  cpu: CpuSpec;
  memory: MemorySpec;
  boot: BootSpec;
  disks: DiskSpec[];
  network_interfaces: NetworkInterfaceSpec[];
  placement: PlacementSpec;
  cloud_init?: CloudInitSpec | null;
  // Machine profile (M15). "microvm" = minimal, fast-booting (direct-kernel,
  // no cloud-init seed, pvpanic, single PCI segment).
  machine_type?: MachineType;
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

// Agent auto-enrollment (M16).
export interface EnrollResponse {
  host_id: string;
  token: string;
  bootstrap_url: string | null;
  ca_cert: string;
  expires_in_secs: number;
}

// Host drain result (M15, host lifecycle).
export interface DrainMove {
  vm_id: string;
  vm_name: string;
  target_host_id: string;
  target_host_name: string;
}
export interface DrainSkip {
  vm_id: string;
  vm_name: string;
  reason: string;
}
export interface DrainResult {
  cordoned: boolean;
  migrating: DrainMove[];
  skipped: DrainSkip[];
}

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
  // Cross-CPU migration (M15): vendor + curated guest-visible ISA flags.
  cpu_vendor: string | null;
  cpu_features: string[];
  total_memory_bytes: number | null;
  available_memory_bytes: number | null;
  vm_count: number;
  last_heartbeat: string | null;
  created_at: string;
  updated_at: string;
  generation: number;
}

/// What a network is, and therefore what it isolates (design §18, ADR-016).
/// `provider` and `vlan` attach to physical infrastructure and are
/// platform-only; `tenant` is a self-contained VXLAN overlay.
export type NetworkKind = "provider" | "vlan" | "tenant";

export interface Network {
  id: string;
  name: string;
  kind: NetworkKind;
  /// Uplink a physical (provider/vlan) network attaches to.
  physical_network: string | null;
  /// The L2 segment this network occupies, unique fleet-wide.
  segment_key: string | null;
  /// Predates the kind model: its segment is not guaranteed distinct, so it may
  /// share a broadcast domain with another network.
  legacy_segment: boolean;
  /// Policy applied to every NIC on this network, unioned with the NIC's own
  /// groups (ADR-017).
  default_security_group_id: string | null;
  vlan: number | null;
  // VXLAN overlay (M13b): set ⇒ VNI-isolated overlay spanning hosts.
  vni: number | null;
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

// Security groups (M13c)
export interface SecurityGroupRule {
  id: string;
  security_group_id: string;
  direction: string;
  ethertype: string;
  protocol: string;
  port_min: number | null;
  port_max: number | null;
  remote_cidr: string | null;
  created_at: string;
}

export interface SecurityGroup {
  id: string;
  name: string;
  description: string | null;
  rules: SecurityGroupRule[];
  created_at: string;
  updated_at: string;
}

export interface CreateRuleRequest {
  direction?: string;
  ethertype?: string;
  protocol?: string;
  port_min?: number | null;
  port_max?: number | null;
  remote_cidr?: string | null;
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
  // Lifecycle (M14b)
  status: string; // ready | importing | failed
  managed: boolean;
  size_bytes: number | null;
  error: string | null;
  created_at: string;
  updated_at: string;
}

// ISO available for read-only attachment (M15, Windows guests).
export interface IsoEntry {
  name: string;
  path: string;
  size_bytes: number;
}

export interface ImportImageRequest {
  name: string;
  url: string;
  format: "raw" | "qcow2";
  boot: BootSpec;
  default_size_bytes?: number | null;
  cloud_init?: boolean;
  os?: string | null;
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
  machine_type: MachineType;
  created_at: string;
  updated_at: string;
}

export interface CreateNetworkRequest {
  name: string;
  /// Declares the isolation guarantee. Omitting it defaults to `provider`
  /// server-side, so a VLAN network must say so explicitly or its tag is
  /// rejected (ADR-016).
  kind?: NetworkKind;
  /// Uplink for a physical network. Defaults to `default`.
  physical_network?: string | null;
  vlan?: number | null;
  // Deprecated spelling of kind = "tenant"; kept for older callers.
  overlay?: boolean;
  // Rejected by the control plane — a VNI is never caller-supplied.
  vni?: number | null;
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
  machine_type?: MachineType;
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
  /** Effective permissions **in `project`** — not globally. */
  permissions: string[];
  /** The project this answer is about; absent in the platform view. */
  project?: string | null;
  /** Whether requests are project-scoped at all. */
  tenancy: boolean;
  /** Whether the caller may take the cross-project view. */
  platform: boolean;
}

/// Per-project limits (ADR-019). A null field is unlimited in that dimension.
export interface QuotaLimits {
  max_vms?: number | null;
  max_vcpus?: number | null;
  max_memory_mib?: number | null;
  max_volumes?: number | null;
  max_storage_bytes?: number | null;
}

/// What a project is using, derived from the owning tables — never stored.
export interface QuotaUsage {
  vms: number;
  vcpus: number;
  memory_mib: number;
  volumes: number;
  storage_bytes: number;
}

export interface QuotaView {
  limits: QuotaLimits;
  usage: QuotaUsage;
  /// Usage already past a limit — which happens when a limit is lowered, and is
  /// permitted. New commitments are refused; nothing is destroyed.
  over_quota: boolean;
}

export interface Project {
  id: string;
  name: string;
  description?: string | null;
  is_default: boolean;
  created_at: string;
  updated_at: string;
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

// Volumes (M14a)
export interface Volume {
  id: string;
  name: string;
  size_bytes: number;
  format: string;
  attached_vm_id: string | null;
  attached_serial: number | null;
  source_image_id: string | null; // set ⇒ bootable (M14d)
  path: string;
  created_at: string;
  updated_at: string;
}

export interface VolumeSnapshot {
  id: string;
  volume_id: string;
  name: string;
  created_at: string;
}

export interface VmMetrics {
  running: boolean;
  cpu_pct: number;
  mem_bytes: number;
  disk_read_bytes: number;
  disk_write_bytes: number;
  disk_read_ops: number;
  disk_write_ops: number;
  net_rx_bytes: number;
  net_tx_bytes: number;
  net_rx_packets: number;
  net_tx_packets: number;
}
