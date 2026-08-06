import React, { useMemo } from "react";
import ReactDOM from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ThemeProvider } from "@mui/material/styles";
// Self-hosted fonts. The console runs in trusted labs with no egress, and the
// control plane serves it under a same-origin CSP — a webfont CDN would be
// both a broken dependency and a third party watching an operator work.
import "@fontsource-variable/inter-tight/wght.css";
import "@fontsource-variable/inter/wght.css";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/500.css";
import "./styles/tokens.css";
import "./styles/app.css";
import { buildTheme } from "./theme";
import { ThemeModeProvider, useThemeMode } from "./theme/ThemeMode";
import { App } from "./App";
import { AuthProvider } from "./auth/AuthProvider";

const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: 1, refetchOnWindowFocus: false } },
});

// MUI only dresses the overlays, but it still needs to know which theme is
// live so a dialog never arrives in the wrong palette.
function MuiBridge({ children }: { children: React.ReactNode }) {
  const { mode } = useThemeMode();
  const theme = useMemo(() => buildTheme(mode), [mode]);
  return <ThemeProvider theme={theme}>{children}</ThemeProvider>;
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <ThemeModeProvider>
        <MuiBridge>
          <BrowserRouter>
            <AuthProvider>
              <App />
            </AuthProvider>
          </BrowserRouter>
        </MuiBridge>
      </ThemeModeProvider>
    </QueryClientProvider>
  </React.StrictMode>,
);
