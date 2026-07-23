// React Query hooks. Live state is polled on an interval until the event stream
// exists (design section 33: "Initial implementation may poll").

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "./client";
import type {
  Accepted,
  CreateVmRequest,
  Event,
  Host,
  Network,
  Task,
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

export function useDeleteNetwork() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del<void>(`/networks/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["networks"] }),
  });
}
