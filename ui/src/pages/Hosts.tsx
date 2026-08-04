import { useState } from "react";
import Button from "@mui/material/Button";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import Alert from "@mui/material/Alert";
import AddIcon from "@mui/icons-material/Add";
import { DataGrid, type GridColDef } from "@mui/x-data-grid";
import { useHosts, useRegisterHost } from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { StatusChip } from "../components/StatusChip";
import { formatBytes } from "../format";
import type { Host } from "../api/types";

const columns: GridColDef<Host>[] = [
  { field: "name", headerName: "Name", flex: 1, minWidth: 120 },
  {
    field: "state",
    headerName: "Status",
    width: 120,
    renderCell: (p) => <StatusChip value={p.value as string} />,
  },
  {
    field: "logical_cpus",
    headerName: "vCPUs",
    width: 90,
    valueGetter: (_v, row) => row.logical_cpus ?? "—",
  },
  {
    field: "memory",
    headerName: "Memory (used / total)",
    width: 200,
    valueGetter: (_v, row) =>
      row.total_memory_bytes != null && row.available_memory_bytes != null
        ? `${formatBytes(row.total_memory_bytes - row.available_memory_bytes)} / ${formatBytes(row.total_memory_bytes)}`
        : "—",
  },
  { field: "vm_count", headerName: "VMs", width: 80 },
  {
    field: "cloud_hypervisor_version",
    headerName: "CH version",
    width: 120,
    valueGetter: (_v, row) => row.cloud_hypervisor_version ?? "—",
  },
  { field: "endpoint", headerName: "Agent endpoint", flex: 1, minWidth: 180 },
];

function RegisterDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [name, setName] = useState("");
  const [endpoint, setEndpoint] = useState("http://127.0.0.1:9500");
  const register = useRegisterHost();

  const submit = () => {
    register.mutate(
      { name, endpoint },
      {
        onSuccess: () => {
          setName("");
          onClose();
        },
      },
    );
  };

  return (
    <Dialog open={open} onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Register host</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          <TextField
            label="Agent gRPC endpoint"
            value={endpoint}
            onChange={(e) => setEndpoint(e.target.value)}
            helperText="e.g. http://10.0.0.11:9500"
          />
          {register.isError && <Alert severity="error">{(register.error as Error).message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="contained" onClick={submit} disabled={!name || !endpoint || register.isPending}>
          Register
        </Button>
      </DialogActions>
    </Dialog>
  );
}

export function Hosts() {
  const hosts = useHosts();
  const [dialog, setDialog] = useState(false);
  const { can } = usePermissions();

  return (
    <Stack spacing={2}>
      <Stack direction="row" alignItems="center" justifyContent="space-between">
        <Typography variant="h5">Hosts</Typography>
        {can("host:manage") && (
          <Button variant="contained" startIcon={<AddIcon />} onClick={() => setDialog(true)}>
            Register host
          </Button>
        )}
      </Stack>
      {hosts.isError && <Alert severity="error">{(hosts.error as Error).message}</Alert>}
      <div style={{ height: 520, width: "100%" }}>
        <DataGrid
          rows={hosts.data ?? []}
          columns={columns}
          loading={hosts.isLoading}
          density="compact"
          disableRowSelectionOnClick
          initialState={{ pagination: { paginationModel: { pageSize: 25 } } }}
          pageSizeOptions={[10, 25, 50]}
        />
      </div>
      <RegisterDialog open={dialog} onClose={() => setDialog(false)} />
    </Stack>
  );
}
