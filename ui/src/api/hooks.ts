// React Query hooks. Live state is polled on an interval until the event stream
// exists (design section 33: "Initial implementation may poll").

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "./client";
import type {
  Accepted,
  CreateImageRequest,
  CreateNetworkRequest,
  CreateTemplateRequest,
  CreateVmFromTemplateRequest,
  CreateVmRequest,
  Event,
  Host,
  Image,
  IpAllocation,
  Network,
  Task,
  Template,
  UpdateVmRequest,
  Vm,
} from "./types";

const POLL_MS = 3000;

export function useHosts() {
  return useQuery({
    queryKey: ["hosts"],
    queryFn: () => api.get<Host[]>("/hosts"),
    refetchInterval: POLL_MS,
  });
}

export function useHost(id: string | undefined) {
  return useQuery({
    queryKey: ["hosts", id],
    queryFn: () => api.get<Host>(`/hosts/${id}`),
    refetchInterval: POLL_MS,
    enabled: !!id,
  });
}

export function useVms() {
  return useQuery({
    queryKey: ["vms"],
    queryFn: () => api.get<Vm[]>("/vms"),
    refetchInterval: POLL_MS,
  });
}

export function useVm(id: string | undefined) {
  return useQuery({
    queryKey: ["vms", id],
    queryFn: () => api.get<Vm>(`/vms/${id}`),
    refetchInterval: POLL_MS,
    enabled: !!id,
  });
}

export function useNetworks() {
  return useQuery({
    queryKey: ["networks"],
    queryFn: () => api.get<Network[]>("/networks"),
    refetchInterval: POLL_MS,
  });
}

export function useTasks() {
  return useQuery({
    queryKey: ["tasks"],
    queryFn: () => api.get<Task[]>("/tasks"),
    refetchInterval: POLL_MS,
  });
}

export function useEvents() {
  return useQuery({
    queryKey: ["events"],
    queryFn: () => api.get<Event[]>("/events?limit=200"),
    refetchInterval: POLL_MS,
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
  return useQuery({
    queryKey: ["images"],
    queryFn: () => api.get<Image[]>("/images"),
    refetchInterval: POLL_MS,
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
  return useQuery({
    queryKey: ["templates"],
    queryFn: () => api.get<Template[]>("/templates"),
    refetchInterval: POLL_MS,
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
    mutationFn: ({ id, targetHostId }: { id: string; targetHostId: string }) =>
      api.post<Accepted>(`/vms/${id}/migrate`, { target_host_id: targetHostId }),
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
  return useQuery({
    queryKey: ["networks", id, "allocations"],
    queryFn: () => api.get<IpAllocation[]>(`/networks/${id}/allocations`),
    enabled: !!id,
    refetchInterval: POLL_MS,
  });
}

// --- Security groups (M13c) ---
import type { CreateRuleRequest, SecurityGroup } from "./types";

export function useSecurityGroups() {
  return useQuery({
    queryKey: ["security-groups"],
    queryFn: () => api.get<SecurityGroup[]>("/security-groups"),
    refetchInterval: POLL_MS,
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

// --- Volumes (M14a) ---
import type { Volume } from "./types";

export function useVolumes() {
  return useQuery({
    queryKey: ["volumes"],
    queryFn: () => api.get<Volume[]>("/volumes"),
    refetchInterval: POLL_MS,
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
  return useQuery({
    queryKey: ["volumes", volumeId, "snapshots"],
    queryFn: () => api.get<VolumeSnapshot[]>(`/volumes/${volumeId}/snapshots`),
    enabled: !!volumeId,
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
