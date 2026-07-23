import { createTheme } from "@mui/material/styles";

// A calm, dense enterprise theme (dark) suited to an infrastructure console.
export const theme = createTheme({
  palette: {
    mode: "dark",
    primary: { main: "#4f9cf9" },
    background: { default: "#0e1116", paper: "#161b22" },
  },
  typography: {
    fontSize: 13,
    h5: { fontWeight: 600 },
    h6: { fontWeight: 600 },
  },
  components: {
    MuiCard: { defaultProps: { variant: "outlined" } },
    MuiButton: { defaultProps: { disableElevation: true } },
  },
});
