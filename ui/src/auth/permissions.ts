// Effective-permission hook (design M12b). The control plane enforces every
// permission server-side; this drives UX only — hiding actions the caller
// cannot perform. `/me` returns the union of the caller's role permissions
// (superuser/dev mode returns the full catalog).

import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import type { Me } from "../api/types";

export function useMe() {
  return useQuery({
    queryKey: ["me"],
    queryFn: () => api.get<Me>("/me"),
    staleTime: 60_000,
  });
}

/** Returns `can(permission)` for the current user. Fails closed while loading. */
export function usePermissions(): { can: (perm: string) => boolean; loading: boolean } {
  const { data, isLoading } = useMe();
  const perms = data?.permissions;
  return {
    loading: isLoading,
    can: (perm: string) => !!perms?.includes(perm),
  };
}
