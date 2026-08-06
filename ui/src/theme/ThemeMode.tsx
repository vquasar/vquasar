// Theme is one token set with a [data-theme="light"] override (handoff,
// "Theming — do this first"). The choice is persisted; prefers-color-scheme is
// honoured on the first visit only. The swap is instant — no transition.

import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";

export type Mode = "dark" | "light";

const KEY = "vquasar.theme";

function initialMode(): Mode {
  const stored = localStorage.getItem(KEY);
  if (stored === "dark" || stored === "light") return stored;
  return window.matchMedia?.("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

const Ctx = createContext<{ mode: Mode; setMode: (m: Mode) => void }>({
  mode: "dark",
  setMode: () => {},
});

export function ThemeModeProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<Mode>(initialMode);

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", mode);
  }, [mode]);

  const setMode = useCallback((m: Mode) => {
    localStorage.setItem(KEY, m);
    setModeState(m);
  }, []);

  return <Ctx.Provider value={{ mode, setMode }}>{children}</Ctx.Provider>;
}

export function useThemeMode() {
  return useContext(Ctx);
}
