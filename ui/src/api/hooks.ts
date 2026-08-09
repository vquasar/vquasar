// React Query hooks. Live state is polled on an interval until the event stream
// exists (design section 33: "Initial implementation may poll").
//
// Two rules keep the polling honest at fleet scale:
//
//   * Poll fast only while something is actually moving. The console shell
//     mounts every list (the sidebar carries live counts), so a flat interval
//     meant one full fetch of hosts + VMs + volumes + images + templates +
//     networks + security groups + tasks every few seconds, from every open
//     browser, forever. Hosts, VMs and tasks drop to 2s while a task is running
//     or a VM is in a transitional phase, and back to 10s when the fleet is
//     idle. Everything an operator changes by hand polls at 60s and is
//     invalidated by its own mutations.
//   * Never issue a query the caller has no permission to read. Without this a
//     `viewer` scoped to vm:read generates a 403 for volumes, images and
//     templates on every tick.
//
// React Query does not refetch on an interval while the tab is in the
// background, so a console left open on another desktop costs nothing.

import { useMutation, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { api, uploadImage } from "./client";
import { usePermissions } from "../auth/permissions";
import { READ } from "../auth/perm";
import type {
  Accepted,
  CreateImageRequest,
  CreateNetworkRequest,
  CreateTemplateRequest,
  CreateVmFromTemplateRequest,
  CreateVmRequest,
  DrainResult,
  EnrollResponse,
  Event,
  Host,
  Image,
  IsoEntry,
  IpAllocation,
  Network,
  Project,
  QuotaLimits,
  QuotaView,
  Task,
  Template,
  UpdateVmRequest,
  Vm,
  VmMetrics,
  VmPhase,
} from "./types";

/// Something is in flight — match the progress bars an operator is watching.
const FAST_MS = 2_000;
/// The fleet is idle; these still drift on their own (heartbeats, guest IPs).
const STEADY_MS = 10_000;
/// Changes only when somebody acts, and every action invalidates its own key.
const SLOW_MS = 60_000;

/// Phases that mean the control plane is mid-operation on a guest.
const BUSY_PHASES = new Set<VmPhase>([
  "Pending",
  "Scheduling",
  "Creating",
  "Starting",
  "Stopping",
  "Migrating",
  "Deleting",
]);

function tasksBusy(tasks: Task[] | undefined): boolean {
  return !!tasks?.some((t) => t.state === "Running" || t.state === "Pending");
}

/// Tasks drive their own cadence: a running task is exactly the thing whose
/// progress bar must not lie.
function taskInterval(data: Task[] | undefined): number {
  return tasksBusy(data) ? FAST_MS : STEADY_MS;
}

/// Hosts have no busy flag of their own, so they follow the task queue — a
/// drain or a migration is what makes host state worth watching closely.
function busyFromCache(qc: QueryClient): boolean {
  return tasksBusy(qc.getQueryData<Task[]>(["tasks"]));
}

/// The projects this caller can act in. Already scoped server-side to the ones
/// they hold a binding in — which projects exist is itself tenancy information
/// (ADR-020), so this list is not a fleet inventory.
export function useProjects(enabled = true) {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["projects"],
    queryFn: () => api.get<Project[]>("/projects"),
    enabled: enabled && can(READ.projects),
    staleTime: SLOW_MS,
  });
}

export function useCreateProject() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: { name: string; description?: string | null }) =>
      api.post<Project>("/projects", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["projects"] }),
  });
}

export function useUpdateProject() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: { name: string; description?: string | null } }) =>
      api.patch<Project>(`/projects/${id}`, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["projects"] }),
  });
}

export function useDeleteProject() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del<void>(`/projects/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["projects"] }),
  });
}

/// A project's limits beside what it is using. Fetched per project rather than
/// as a list because the control plane derives usage on demand — asking for
/// every project's usage at once would aggregate the whole fleet on each poll.
export function useQuota(projectId: string | undefined) {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["quota", projectId],
    queryFn: () => api.get<QuotaView>(`/projects/${projectId}/quota`),
    enabled: !!projectId && can(READ.projects),
    staleTime: SLOW_MS,
  });
}

export function useSetQuota() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: QuotaLimits }) =>
      api.put<QuotaView>(`/projects/${id}/quota`, body),
    onSuccess: (_d, v) => qc.invalidateQueries({ queryKey: ["quota", v.id] }),
  });
}

export function useClearQuota() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del<void>(`/projects/${id}/quota`),
    onSuccess: (_d, id) => qc.invalidateQueries({ queryKey: ["quota", id] }),
  });
}

export function useHosts() {
  const { can } = usePermissions();
  const qc = useQueryClient();
  return useQuery({
    queryKey: ["hosts"],
    queryFn: () => api.get<Host[]>("/hosts"),
    enabled: can(READ.hosts),
    refetchInterval: () => (busyFromCache(qc) ? FAST_MS : STEADY_MS),
  });
}

export function useHost(id: string | undefined) {
  const { can } = usePermissions();
  const qc = useQueryClient();
  return useQuery({
    queryKey: ["hosts", id],
    queryFn: () => api.get<Host>(`/hosts/${id}`),
    refetchInterval: () => (busyFromCache(qc) ? FAST_MS : STEADY_MS),
    enabled: !!id && can(READ.hosts),
  });
}

export function useVms() {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["vms"],
    queryFn: () => api.get<Vm[]>("/vms"),
    enabled: can(READ.vms),
    refetchInterval: (q) =>
      q.state.data?.some((v) => BUSY_PHASES.has(v.phase)) ? FAST_MS : STEADY_MS,
  });
}

export function useVm(id: string | undefined) {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["vms", id],
    queryFn: () => api.get<Vm>(`/vms/${id}`),
    refetchInterval: (q) => (q.state.data && BUSY_PHASES.has(q.state.data.phase) ? FAST_MS : STEADY_MS),
    enabled: !!id && can(READ.vms),
  });
}

export function useNetworks() {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["networks"],
    queryFn: () => api.get<Network[]>("/networks"),
    enabled: can(READ.networks),
    refetchInterval: SLOW_MS,
  });
}

export function useTasks() {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["tasks"],
    queryFn: () => api.get<Task[]>("/tasks"),
    enabled: can(READ.tasks),
    refetchInterval: (q) => taskInterval(q.state.data),
  });
}

export function useEvents() {
  const { can } = usePermissions();
  const qc = useQueryClient();
  return useQuery({
    queryKey: ["events"],
    queryFn: () => api.get<Event[]>("/events?limit=200"),
    enabled: can(READ.events),
    refetchInterval: () => (busyFromCache(qc) ? FAST_MS : STEADY_MS),
  });
}

function useInvalidate() {
  const qc = useQueryClient();
  return () => {
    qc.invalidateQueries({ queryKey: ["vms"] });
    qc.invalidateQueries({ queryKey: ["tasks"] });
    qc.invalidateQueries({ queryKey: ["events"] });
  };
}

export function useCreateVm() {
  const invalidate = useInvalidate();
  return useMutation({
    mutationFn: (body: CreateVmRequest) => api.post<Accepted>("/vms", body),
    onSuccess: invalidate,
  });
}

export function useCreateVmFromTemplate() {
  const invalidate = useInvalidate();
  return useMutation({
    mutationFn: (body: CreateVmFromTemplateRequest) =>
      api.post<Accepted>("/vms/from-template", body),
    onSuccess: invalidate,
  });
}

export function useUpdateVm() {
  const invalidate = useInvalidate();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: UpdateVmRequest }) =>
      api.patch<Accepted>(`/vms/${id}`, body),
    onSuccess: invalidate,
  });
}

// ---- images & templates (design M9) --------------------------------------

export function useImages() {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["images"],
    queryFn: () => api.get<Image[]>("/images"),
    enabled: can(READ.images),
    // An import in flight is the one thing here worth watching closely.
    refetchInterval: (q) =>
      q.state.data?.some((i) => i.status === "importing") ? FAST_MS : SLOW_MS,
  });
}

export function useIsos() {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["isos"],
    queryFn: () => api.get<IsoEntry[]>("/isos"),
    enabled: can(READ.images),
    staleTime: SLOW_MS,
  });
}

export function useCreateImage() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateImageRequest) => api.post<Image>("/images", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["images"] }),
  });
}

export function useImportImage() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: import("./types").ImportImageRequest) =>
      api.post<Image>("/images/import", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["images"] }),
  });
}

export function useUploadImage() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ params, file }: { params: Record<string, string>; file: File }) =>
      uploadImage(params, file),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["images"] }),
  });
}

export function useUpdateImage() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: CreateImageRequest }) =>
      api.patch<Image>(`/images/${id}`, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["images"] }),
  });
}

export function useDeleteImage() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del<void>(`/images/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["images"] }),
  });
}

export function useTemplates() {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["templates"],
    queryFn: () => api.get<Template[]>("/templates"),
    enabled: can(READ.templates),
    refetchInterval: SLOW_MS,
  });
}

export function useCreateTemplate() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateTemplateRequest) => api.post<Template>("/templates", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["templates"] }),
  });
}

export function useUpdateTemplate() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: CreateTemplateRequest }) =>
      api.patch<Template>(`/templates/${id}`, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["templates"] }),
  });
}

export function useDeleteTemplate() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del<void>(`/templates/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["templates"] }),
  });
}

export function useVmAction() {
  const invalidate = useInvalidate();
  return useMutation({
    mutationFn: ({ id, action }: { id: string; action: "start" | "stop" | "delete" }) =>
      action === "delete"
        ? api.del<Accepted>(`/vms/${id}`)
        : api.post<Accepted>(`/vms/${id}/${action}`),
    onSuccess: invalidate,
  });
}

export function useMigrateVm() {
  const invalidate = useInvalidate();
  return useMutation({
    mutationFn: ({ id, targetHostId, force }: { id: string; targetHostId: string; force?: boolean }) =>
      api.post<Accepted>(`/vms/${id}/migrate`, { target_host_id: targetHostId, force: force ?? false }),
    onSuccess: invalidate,
  });
}

export function useRegisterHost() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: { name: string; endpoint: string }) => api.post<Host>("/hosts", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["hosts"] }),
  });
}

// Agent auto-enrollment (M16): mint a one-time join token.
export function useEnrollHost() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: { name: string; endpoint: string }) =>
      api.post<EnrollResponse>("/hosts/enroll", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["hosts"] }),
  });
}

// Host lifecycle (M15): cordon/uncordon + drain.
export function useSetHostSchedulable() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, schedulable }: { id: string; schedulable: boolean }) =>
      api.patch<Host>(`/hosts/${id}`, { schedulable }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["hosts"] }),
  });
}

export function useDrainHost() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.post<DrainResult>(`/hosts/${id}/drain`, {}),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["hosts"] }),
  });
}

export function useCreateNetwork() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: { name: string; vlan?: number | null }) =>
      api.post<Network>("/networks", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["networks"] }),
  });
}

export function useUpdateNetwork() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: CreateNetworkRequest }) =>
      api.patch<Network>(`/networks/${id}`, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["networks"] }),
  });
}

export function useDeleteNetwork() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del<void>(`/networks/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["networks"] }),
  });
}

export function useNetworkAllocations(id: string | undefined) {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["networks", id, "allocations"],
    queryFn: () => api.get<IpAllocation[]>(`/networks/${id}/allocations`),
    enabled: !!id && can(READ.networks),
    refetchInterval: SLOW_MS,
  });
}

// --- Security groups (M13c) ---
import type { CreateRuleRequest, SecurityGroup } from "./types";

export function useSecurityGroups() {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["security-groups"],
    queryFn: () => api.get<SecurityGroup[]>("/security-groups"),
    enabled: can(READ.securityGroups),
    refetchInterval: SLOW_MS,
  });
}

export function useCreateSecurityGroup() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: { name: string; description?: string | null }) =>
      api.post<SecurityGroup>("/security-groups", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["security-groups"] }),
  });
}

export function useDeleteSecurityGroup() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del<void>(`/security-groups/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["security-groups"] }),
  });
}

export function useAddSgRule() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: CreateRuleRequest }) =>
      api.post(`/security-groups/${id}/rules`, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["security-groups"] }),
  });
}

export function useDeleteSgRule() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, ruleId }: { id: string; ruleId: string }) =>
      api.del<void>(`/security-groups/${id}/rules/${ruleId}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["security-groups"] }),
  });
}

export function useChangeNic() {
  const invalidate = useInvalidate();
  return useMutation({
    mutationFn: ({
      id,
      index,
      networkId,
      securityGroups,
    }: {
      id: string;
      index: number;
      networkId: string;
      securityGroups?: string[];
    }) =>
      api.put<Accepted>(`/vms/${id}/nics/${index}`, {
        network_id: networkId,
        ...(securityGroups ? { security_groups: securityGroups } : {}),
      }),
    onSuccess: invalidate,
  });
}

// --- Storage pools (ADR-023) ---
import type { StoragePool, StoragePoolDetail } from "./types";

export function useStoragePools() {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["storage-pools"],
    queryFn: () => api.get<StoragePool[]>("/storage-pools"),
    enabled: can(READ.storagePools),
    // A pool's state and free space come from the agents, so this moves on its
    // own without anybody touching the console.
    refetchInterval: SLOW_MS,
  });
}

/// One pool with every host's report. Separate from the list because the
/// per-host detail is what an operator opens *after* something looks wrong.
export function useStoragePool(id: string | undefined) {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["storage-pools", id],
    queryFn: () => api.get<StoragePoolDetail>(`/storage-pools/${id}`),
    enabled: !!id && can(READ.storagePools),
    refetchInterval: SLOW_MS,
  });
}

export function useCreateStoragePool() {
  const qc = useQueryClient();
  return useMutation({
    // The kind-specific fields are flattened into the body, so this is the
    // union of what any kind takes rather than a nested params object.
    mutationFn: (body: {
      name: string;
      kind: string;
      description?: string | null;
      path?: string;
      server?: string;
      export?: string;
      mount_point?: string;
      options?: string;
    }) => api.post<StoragePool>("/storage-pools", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["storage-pools"] }),
  });
}

export function useDeleteStoragePool() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del(`/storage-pools/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["storage-pools"] }),
  });
}

// --- Volumes (M14a) ---
import type { Volume } from "./types";

export function useVolumes() {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["volumes"],
    queryFn: () => api.get<Volume[]>("/volumes"),
    enabled: can(READ.volumes),
    refetchInterval: SLOW_MS,
  });
}

export function useCreateVolume() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: {
      name: string;
      size_bytes: number;
      format: string;
      source_image_id?: string | null;
      /// Which pool to place it in, by id or name (ADR-023). Omitted means
      /// `default`, which is where an existing cluster's volumes already are.
      pool?: string;
    }) => api.post<Volume>("/volumes", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["volumes"] }),
  });
}

export function useCreateVmFromVolume() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: {
      name: string;
      volume_id: string;
      boot_vcpus: number;
      max_vcpus: number;
      memory_mib: number;
      network_id?: string | null;
      security_groups?: string[];
      cloud_init?: import("./types").CloudInitSpec | null;
    }) => api.post<Accepted>("/vms/from-volume", body),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["volumes"] });
      qc.invalidateQueries({ queryKey: ["vms"] });
    },
  });
}

export function useDeleteVolume() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del<void>(`/volumes/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["volumes"] }),
  });
}

export function useAttachVolume() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, vmId }: { id: string; vmId: string }) =>
      api.post<Volume>(`/volumes/${id}/attach`, { vm_id: vmId }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["volumes"] });
      qc.invalidateQueries({ queryKey: ["vms"] });
    },
  });
}

export function useDetachVolume() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.post<Volume>(`/volumes/${id}/detach`),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["volumes"] });
      qc.invalidateQueries({ queryKey: ["vms"] });
    },
  });
}

// --- Volume snapshots (M14c) ---
import type { VolumeSnapshot } from "./types";

export function useVolumeSnapshots(volumeId: string | undefined) {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["volumes", volumeId, "snapshots"],
    queryFn: () => api.get<VolumeSnapshot[]>(`/volumes/${volumeId}/snapshots`),
    enabled: !!volumeId && can(READ.volumes),
  });
}

export function useCreateSnapshot() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ volumeId, name }: { volumeId: string; name: string }) =>
      api.post<VolumeSnapshot>(`/volumes/${volumeId}/snapshots`, { name }),
    onSuccess: (_d, v) => qc.invalidateQueries({ queryKey: ["volumes", v.volumeId, "snapshots"] }),
  });
}

export function useDeleteSnapshot() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ volumeId, snapId }: { volumeId: string; snapId: string }) =>
      api.del<void>(`/volumes/${volumeId}/snapshots/${snapId}`),
    onSuccess: (_d, v) => qc.invalidateQueries({ queryKey: ["volumes", v.volumeId, "snapshots"] }),
  });
}

export function useRevertSnapshot() {
  return useMutation({
    mutationFn: ({ volumeId, snapId }: { volumeId: string; snapId: string }) =>
      api.post<void>(`/volumes/${volumeId}/snapshots/${snapId}/revert`),
  });
}

// --- Per-VM metrics (M15a) ---
/// Only mounted on an open VM detail page, so a short interval here costs one
/// request per watching operator rather than one per VM in the fleet.
export function useVmMetrics(id: string | undefined) {
  const { can } = usePermissions();
  return useQuery({
    queryKey: ["vms", id, "metrics"],
    queryFn: () => api.get<VmMetrics>(`/vms/${id}/metrics`),
    enabled: !!id && can(READ.vms),
    refetchInterval: FAST_MS,
  });
}
