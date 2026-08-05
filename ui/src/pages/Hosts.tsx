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
import Chip from "@mui/material/Chip";
import AddIcon from "@mui/icons-material/Add";
import { DataGrid, GridActionsCellItem, type GridColDef } from "@mui/x-data-grid";
import BlockIcon from "@mui/icons-material/Block";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import CleaningServicesIcon from "@mui/icons-material/CleaningServices";
import { useDrainHost, useHosts, useRegisterHost, useSetHostSchedulable } from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { StatusChip } from "../components/StatusChip";
import { formatBytes } from "../format";
import type { DrainResult, Host } from "../api/types";

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

function DrainResultDialog({ result, onClose }: { result: DrainResult | null; onClose: () => void }) {
  if (!result) return null;
  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Drain started</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <Typography>
            Host cordoned. {result.migrating.length} VM(s) migrating, {result.skipped.length}{" "}
            left in place.
          </Typography>
          {result.migrating.length > 0 && (
            <div>
              <Typography variant="subtitle2">Migrating</Typography>
              {result.migrating.map((m) => (
                <Typography key={m.vm_id} variant="body2" color="text.secondary">
                  {m.vm_name} → {m.target_host_name}
                </Typography>
              ))}
            </div>
          )}
          {result.skipped.length > 0 && (
            <Alert severity="warning">
              {result.skipped.map((s) => (
                <div key={s.vm_id}>
                  {s.vm_name}: {s.reason}
                </div>
              ))}
            </Alert>
          )}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button variant="contained" onClick={onClose}>
          Close
        </Button>
      </DialogActions>
    </Dialog>
  );
}

export function Hosts() {
  const hosts = useHosts();
  const [dialog, setDialog] = useState(false);
  const { can } = usePermissions();
  const setSchedulable = useSetHostSchedulable();
  const drain = useDrainHost();
  const [drainResult, setDrainResult] = useState<DrainResult | null>(null);
  const manage = can("host:manage");

  const columns: GridColDef<Host>[] = [
    { field: "name", headerName: "Name", flex: 1, minWidth: 120 },
    {
      field: "state",
      headerName: "Status",
      width: 110,
      renderCell: (p) => <StatusChip value={p.value as string} />,
    },
    {
      field: "schedulable",
      headerName: "Scheduling",
      width: 130,
      renderCell: (p) =>
        p.value ? (
          <Chip size="small" color="success" variant="outlined" label="Schedulable" />
        ) : (
          <Chip size="small" color="warning" variant="outlined" label="Cordoned" />
        ),
    },
    {
      field: "logical_cpus",
      headerName: "vCPUs",
      width: 80,
      valueGetter: (_v, row) => row.logical_cpus ?? "—",
    },
    {
      field: "memory",
      headerName: "Memory (used / total)",
      width: 190,
      valueGetter: (_v, row) =>
        row.total_memory_bytes != null && row.available_memory_bytes != null
          ? `${formatBytes(row.total_memory_bytes - row.available_memory_bytes)} / ${formatBytes(row.total_memory_bytes)}`
          : "—",
    },
    { field: "vm_count", headerName: "VMs", width: 70 },
    {
      field: "cloud_hypervisor_version",
      headerName: "CH version",
      width: 110,
      valueGetter: (_v, row) => row.cloud_hypervisor_version ?? "—",
    },
    { field: "endpoint", headerName: "Agent endpoint", flex: 1, minWidth: 160 },
    ...(manage
      ? [
          {
            field: "actions",
            type: "actions",
            headerName: "",
            width: 60,
            getActions: (p) =>
              p.row.schedulable
                ? [
                    <GridActionsCellItem
                      key="cordon"
                      icon={<BlockIcon />}
                      label="Cordon (maintenance)"
                      onClick={() => setSchedulable.mutate({ id: p.row.id, schedulable: false })}
                      showInMenu
                    />,
                    <GridActionsCellItem
                      key="drain"
                      icon={<CleaningServicesIcon />}
                      label="Drain (evacuate VMs)"
                      onClick={() =>
                        drain.mutate(p.row.id, { onSuccess: (r) => setDrainResult(r) })
                      }
                      showInMenu
                    />,
                  ]
                : [
                    <GridActionsCellItem
                      key="uncordon"
                      icon={<CheckCircleIcon />}
                      label="Uncordon"
                      onClick={() => setSchedulable.mutate({ id: p.row.id, schedulable: true })}
                      showInMenu
                    />,
                    <GridActionsCellItem
                      key="drain"
                      icon={<CleaningServicesIcon />}
                      label="Drain (evacuate VMs)"
                      onClick={() =>
                        drain.mutate(p.row.id, { onSuccess: (r) => setDrainResult(r) })
                      }
                      showInMenu
                    />,
                  ],
          } as GridColDef<Host>,
        ]
      : []),
  ];

  return (
    <Stack spacing={2}>
      <Stack direction="row" alignItems="center" justifyContent="space-between">
        <Typography variant="h5">Hosts</Typography>
        {manage && (
          <Button variant="contained" startIcon={<AddIcon />} onClick={() => setDialog(true)}>
            Register host
          </Button>
        )}
      </Stack>
      {hosts.isError && <Alert severity="error">{(hosts.error as Error).message}</Alert>}
      {drain.isError && <Alert severity="error">{(drain.error as Error).message}</Alert>}
      {setSchedulable.isError && (
        <Alert severity="error">{(setSchedulable.error as Error).message}</Alert>
      )}
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
      <DrainResultDialog result={drainResult} onClose={() => setDrainResult(null)} />
    </Stack>
  );
}
