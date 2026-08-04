// React Query hooks for the IAM surface (design M12b).

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "./client";
import type {
  CreateRoleRequest,
  GroupRoleView,
  RoleView,
  UpdateRoleRequest,
  UserView,
} from "./types";

export function useIamUsers() {
  return useQuery({ queryKey: ["iam", "users"], queryFn: () => api.get<UserView[]>("/users") });
}

export function useIamRoles() {
  return useQuery({ queryKey: ["iam", "roles"], queryFn: () => api.get<RoleView[]>("/roles") });
}

export function usePermissionCatalog() {
  return useQuery({
    queryKey: ["iam", "permissions"],
    queryFn: () => api.get<string[]>("/permissions"),
    staleTime: Infinity,
  });
}

export function useGroupMappings() {
  return useQuery({
    queryKey: ["iam", "group-mappings"],
    queryFn: () => api.get<GroupRoleView[]>("/group-mappings"),
  });
}

export function useSetUserRoles() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ userId, roleIds }: { userId: string; roleIds: string[] }) =>
      api.put<UserView>(`/users/${userId}/roles`, { role_ids: roleIds }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["iam", "users"] }),
  });
}

export function useCreateRole() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateRoleRequest) => api.post<RoleView>("/roles", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["iam", "roles"] }),
  });
}

export function useUpdateRole() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: UpdateRoleRequest }) =>
      api.patch<RoleView>(`/roles/${id}`, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["iam", "roles"] }),
  });
}

export function useDeleteRole() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del<void>(`/roles/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["iam", "roles"] }),
  });
}

export function useAddGroupMapping() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ group, roleId }: { group: string; roleId: string }) =>
      api.post<void>("/group-mappings", { group, role_id: roleId }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["iam", "group-mappings"] }),
  });
}

export function useRemoveGroupMapping() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ group, roleId }: { group: string; roleId: string }) =>
      api.del<void>(`/group-mappings/${encodeURIComponent(group)}/${roleId}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["iam", "group-mappings"] }),
  });
}
