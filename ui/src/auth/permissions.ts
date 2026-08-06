// Effective-permission hook (design M12b). The control plane enforces every
// permission server-side; this drives UX only — hiding actions the caller
// cannot perform. `/me` returns the union of the caller's role permissions
// (superuser/dev mode returns the full catalog).

import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import type { Me } from "../api/types";
import type { Permission } from "./perm";

export function useMe() {
  return useQuery({
    queryKey: ["me"],
    queryFn: () => api.get<Me>("/me"),
    staleTime: 60_000,
  });
}

/** Returns `can(permission)` for the current user. Fails closed while loading. */
export function usePermissions(): {
  can: (perm: Permission) => boolean;
  loading: boolean;
} {
  const { data, isLoading } = useMe();
  const perms = data?.permissions;
  return {
    loading: isLoading,
    // Exact match only. The catalog carries no wildcards — a superuser is
    // expanded to the full catalog server-side before it reaches us — so
    // treating "*" as a match here would invent authority the server never
    // granted.
    can: (perm: Permission) => !!perms?.includes(perm),
  };
}
