import React, { useMemo } from "react";
import ReactDOM from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ThemeProvider } from "@mui/material/styles";
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
