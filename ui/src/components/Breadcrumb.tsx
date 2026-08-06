// A detail route knows its resource's name; the shell does not. Rather than
// let the top bar print a UUID, a page publishes its own crumb label here and
// the breadcrumb reads it.

import { createContext, useContext, useEffect, useState, type ReactNode } from "react";

const Ctx = createContext<{
  label: string | null;
  setLabel: (l: string | null) => void;
}>({ label: null, setLabel: () => {} });

export function CrumbProvider({ children }: { children: ReactNode }) {
  const [label, setLabel] = useState<string | null>(null);
  return <Ctx.Provider value={{ label, setLabel }}>{children}</Ctx.Provider>;
}

export function useCrumbLabel() {
  return useContext(Ctx).label;
}

/// Publish the current screen's crumb. Pass undefined while the resource is
/// still loading; the shell falls back to the route name.
export function useCrumb(label: string | null | undefined) {
  const { setLabel } = useContext(Ctx);
  useEffect(() => {
    setLabel(label ?? null);
    return () => setLabel(null);
  }, [label, setLabel]);
}
