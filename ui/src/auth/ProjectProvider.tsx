// The project the console is acting in (design §47, ADR-018/020).
//
// One selection, applied to every request by `client.ts`, because a console
// showing one project's VMs beside another project's networks would be worse
// than not having projects at all.
//
// The selection survives a reload (localStorage) but is validated against what
// the caller can actually act in: a binding removed while a tab was closed must
// not leave that tab pinned to a project it will now be refused from.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { useQueryClient } from "@tanstack/react-query";
import { setProjectGetter } from "../api/client";
import { useMe } from "./permissions";
import { useProjects } from "../api/hooks";

/** The header value meaning "every project". Not a project id. */
export const PLATFORM = "*";

const STORAGE_KEY = "vquasar.project";

interface ProjectContext {
  /** Header value in force: a project id, `"*"`, or null (send no header). */
  project: string | null;
  setProject: (next: string | null) => void;
  /** Whether the control plane scopes requests at all. */
  enabled: boolean;
  /** Whether this caller may take the platform view. */
  canPlatform: boolean;
}

const Ctx = createContext<ProjectContext>({
  project: null,
  setProject: () => {},
  enabled: false,
  canPlatform: false,
});

// Registered once, at module scope, so the getter is in place before the first
// request rather than after the provider's first effect runs.
let selected: string | null = readStored();
setProjectGetter(() => selected);

function readStored(): string | null {
  try {
    return localStorage.getItem(STORAGE_KEY);
  } catch {
    return null; // private mode, or storage disabled
  }
}

export function ProjectProvider({ children }: { children: ReactNode }) {
  const [project, setState] = useState<string | null>(selected);
  const qc = useQueryClient();
  const { data: me } = useMe();
  const enabled = me?.tenancy ?? false;
  const canPlatform = me?.platform ?? false;
  const { data: projects } = useProjects(enabled);

  const setProject = useCallback(
    (next: string | null) => {
      if (next === selected) return;
      selected = next;
      setState(next);
      try {
        if (next) localStorage.setItem(STORAGE_KEY, next);
        else localStorage.removeItem(STORAGE_KEY);
      } catch {
        /* storage unavailable; the selection still applies for this session */
      }
      // Every cached answer was scoped to the old project. Invalidating rather
      // than clearing keeps mounted screens showing their skeletons instead of
      // flashing empty, and refetches under the new header.
      void qc.invalidateQueries();
    },
    [qc],
  );

  // Drop a stored selection that no longer resolves — a project deleted, or a
  // binding revoked. Falling back to null means the caller's default project,
  // which is the same thing they would get on a fresh login.
  useEffect(() => {
    if (!enabled || !projects) return;
    if (project === PLATFORM) {
      if (!canPlatform) setProject(null);
      return;
    }
    if (project && !projects.some((p) => p.id === project)) {
      setProject(null);
    }
  }, [enabled, projects, project, canPlatform, setProject]);

  // Tenancy off: never send the header at all, whatever is in storage.
  useEffect(() => {
    if (me && !enabled && project !== null) setProject(null);
  }, [me, enabled, project, setProject]);

  const value = useMemo(
    () => ({ project, setProject, enabled, canPlatform }),
    [project, setProject, enabled, canPlatform],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useProject() {
  return useContext(Ctx);
}
