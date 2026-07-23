import { useState } from "react";
import Alert from "@mui/material/Alert";
import Button from "@mui/material/Button";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import { DataGrid, GridActionsCellItem, type GridColDef } from "@mui/x-data-grid";
import { useCreateNetwork, useDeleteNetwork, useNetworks } from "../api/hooks";
import { formatDate } from "../format";
import type { Network } from "../api/types";

function CreateDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [name, setName] = useState("");
  const [vlan, setVlan] = useState("");
  const create = useCreateNetwork();

  const submit = () =>
    create.mutate(
      { name, vlan: vlan ? Number(vlan) : null },
      {
        onSuccess: () => {
          setName("");
          setVlan("");
          onClose();
        },
      },
    );

  return (
    <Dialog open={open} onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Create network</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          <TextField
            label="VLAN (optional, 1–4094)"
            value={vlan}
            onChange={(e) => setVlan(e.target.value)}
            helperText="Leave blank for a flat provider network"
          />
          {create.isError && <Alert severity="error">{(create.error as Error).message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="contained" onClick={submit} disabled={!name || create.isPending}>
          Create
        </Button>
      </DialogActions>
    </Dialog>
  );
}

export function Networks() {
  const networks = useNetworks();
  const del = useDeleteNetwork();
  const [dialog, setDialog] = useState(false);

  const columns: GridColDef<Network>[] = [
    { field: "name", headerName: "Name", flex: 1, minWidth: 160 },
    {
      field: "vlan",
      headerName: "VLAN",
      width: 120,
      valueGetter: (v) => (v == null ? "flat" : v),
    },
    { field: "created_at", headerName: "Created", width: 200, valueGetter: (v) => formatDate(v as string) },
    {
      field: "actions",
      type: "actions",
      headerName: "",
      width: 60,
      getActions: (p) => [
        <GridActionsCellItem
          key="del"
          icon={<DeleteIcon />}
          label="Delete"
          onClick={() => del.mutate(p.row.id)}
        />,
      ],
    },
  ];

  return (
    <Stack spacing={2}>
      <Stack direction="row" alignItems="center" justifyContent="space-between">
        <Typography variant="h5">Networks</Typography>
        <Button variant="contained" startIcon={<AddIcon />} onClick={() => setDialog(true)}>
          Create network
        </Button>
      </Stack>
      {networks.isError && <Alert severity="error">{(networks.error as Error).message}</Alert>}
      <div style={{ height: 480, width: "100%" }}>
        <DataGrid
          rows={networks.data ?? []}
          columns={columns}
          loading={networks.isLoading}
          density="compact"
          disableRowSelectionOnClick
        />
      </div>
      <CreateDialog open={dialog} onClose={() => setDialog(false)} />
    </Stack>
  );
}
