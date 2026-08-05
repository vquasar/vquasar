// Sign-in gate shown when the control plane enforces authentication and no
// valid session exists (design M12b). Kicks off the OIDC redirect.

import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Paper from "@mui/material/Paper";
import Typography from "@mui/material/Typography";
import Alert from "@mui/material/Alert";
import LoginIcon from "@mui/icons-material/Login";
import ComputerIcon from "@mui/icons-material/Computer";
import { useAuth } from "../auth/AuthProvider";

export function Login() {
  const { login, error } = useAuth();
  return (
    <Box
      sx={{
        minHeight: "100vh",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        p: 2,
      }}
    >
      <Paper sx={{ p: 4, maxWidth: 380, width: "100%", textAlign: "center" }} elevation={3}>
        <ComputerIcon color="primary" sx={{ fontSize: 48, mb: 1 }} />
        <Typography variant="h5" gutterBottom>
          vquasar
        </Typography>
        <Typography variant="body2" color="text.secondary" sx={{ mb: 3 }}>
          Sign in to manage your Cloud Hypervisor cluster.
        </Typography>
        {error && (
          <Alert severity="error" sx={{ mb: 2, textAlign: "left" }}>
            {error}
          </Alert>
        )}
        <Button variant="contained" size="large" startIcon={<LoginIcon />} onClick={login} fullWidth>
          Sign in
        </Button>
      </Paper>
    </Box>
  );
}
