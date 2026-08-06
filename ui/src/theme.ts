import { createTheme } from "@mui/material/styles";
import type { Mode } from "./theme/ThemeMode";

// MUI is kept only for the behaviour-heavy primitives — dialogs and menus,
// where focus management and portalling are worth the dependency (handoff,
// "Implementation approach"). Its palette mirrors the CSS custom properties in
// src/styles/tokens.css so an overlay never looks foreign; everything else in
// the console is a plain element styled from those same tokens.

const DARK = {
  bg: "#0B0F16",
  surface: "#161B22",
  line: "#222A35",
  text: "#F4F7FB",
  text2: "#A6B0BF",
  blue: "#4F9CF9",
  red: "#F2695F",
  amber: "#F5A94A",
  green: "#4FD08A",
};

const LIGHT = {
  bg: "#F1F4F9",
  surface: "#FFFFFF",
  line: "#DFE5EE",
  text: "#0B0F16",
  text2: "#4A5666",
  blue: "#1E6FD9",
  red: "#C0362C",
  amber: "#A66200",
  green: "#0E7A50",
};

export function buildTheme(mode: Mode) {
  const c = mode === "light" ? LIGHT : DARK;
  return createTheme({
    palette: {
      mode,
      primary: { main: c.blue },
      error: { main: c.red },
      warning: { main: c.amber },
      success: { main: c.green },
      divider: c.line,
      background: { default: c.bg, paper: c.surface },
      text: { primary: c.text, secondary: c.text2 },
    },
    shape: { borderRadius: 9 },
    typography: {
      fontFamily: '"Inter Tight", Inter, Helvetica, Arial, sans-serif',
      fontSize: 12.5,
    },
    components: {
      MuiButton: { defaultProps: { disableElevation: true } },
      MuiPaper: { styleOverrides: { root: { backgroundImage: "none" } } },
      MuiDialog: {
        styleOverrides: {
          paper: { border: `1px solid ${c.line}`, borderRadius: 9 },
        },
      },
      MuiMenu: {
        styleOverrides: {
          paper: { border: `1px solid ${c.line}`, borderRadius: 6, minWidth: 170 },
          list: { paddingTop: 4, paddingBottom: 4 },
        },
      },
      MuiMenuItem: { styleOverrides: { root: { fontSize: 12.5, minHeight: 32 } } },
    },
  });
}
