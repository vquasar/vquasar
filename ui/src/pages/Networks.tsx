import { useState } from "react";
import Alert from "@mui/material/Alert";
import Button from "@mui/material/Button";
import Chip from "@mui/material/Chip";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import Divider from "@mui/material/Divider";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import EditIcon from "@mui/icons-material/Edit";
import LanIcon from "@mui/icons-material/Lan";
import { DataGrid, GridActionsCellItem, type GridColDef } from "@mui/x-data-grid";
import {
  useCreateNetwork,
  useDeleteNetwork,
  useNetworkAllocations,
  useNetworks,
  useUpdateNetwork,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { formatDate } from "../format";
import type { CreateNetworkRequest, Network } from "../api/types";

const empty = (s: string) => (s.trim() === "" ? null : s.trim());

function EditDialog({ edit, onClose }: { edit: Network | null; onClose: () => void }) {
  const create = useCreateNetwork();
  const update = useUpdateNetwork();
  const [name, setName] = useState(edit?.name ?? "");
  const [vlan, setVlan] = useState(edit?.vlan != null ? String(edit.vlan) : "");
  const [cidr4, setCidr4] = useState(edit?.cidr_v4 ?? "");
  const [gw4, setGw4] = useState(edit?.gateway_v4 ?? "");
  const [cidr6, setCidr6] = useState(edit?.cidr_v6 ?? "");
  const [gw6, setGw6] = useState(edit?.gateway_v6 ?? "");
  const [dns, setDns] = useState((edit?.dns ?? []).join(", "));

  const submit = () => {
    const body: CreateNetworkRequest = {
      name,
      vlan: vlan ? Number(vlan) : null,
      cidr_v4: empty(cidr4),
      gateway_v4: empty(gw4),
      cidr_v6: empty(cidr6),
      gateway_v6: empty(gw6),
      dns: dns
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean),
    };
    if (edit) update.mutate({ id: edit.id, body }, { onSuccess: onClose });
    else create.mutate(body, { onSuccess: onClose });
  };
  const busy = create.isPending || update.isPending;
  const err = (create.error || update.error) as Error | null;

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>{edit ? "Edit network" : "Create network"}</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          <TextField
            label="VLAN (optional, 1–4094)"
            value={vlan}
            onChange={(e) => setVlan(e.target.value)}
            helperText="Leave blank for a flat provider network"
          />
          <Divider textAlign="left">
            <Typography variant="caption" color="text.secondary">
              IP management (leave a family blank for DHCP)
            </Typography>
          </Divider>
          <Stack direction="row" spacing={2}>
            <TextField
              label="IPv4 subnet (CIDR)"
              value={cidr4}
              onChange={(e) => setCidr4(e.target.value)}
              placeholder="192.168.222.0/24"
              fullWidth
            />
            <TextField
              label="IPv4 gateway"
              value={gw4}
              onChange={(e) => setGw4(e.target.value)}
              placeholder="192.168.222.1"
              fullWidth
            />
          </Stack>
          <Stack direction="row" spacing={2}>
            <TextField
              label="IPv6 subnet (CIDR)"
              value={cidr6}
              onChange={(e) => setCidr6(e.target.value)}
              placeholder="fd00:56::/64"
              fullWidth
            />
            <TextField
              label="IPv6 gateway"
              value={gw6}
              onChange={(e) => setGw6(e.target.value)}
              placeholder="fd00:56::1"
              fullWidth
            />
          </Stack>
          <TextField
            label="DNS servers (comma-separated IPs)"
            value={dns}
            onChange={(e) => setDns(e.target.value)}
            placeholder="1.1.1.1, 8.8.8.8"
          />
          {err && <Alert severity="error">{err.message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="contained" onClick={submit} disabled={!name || busy}>
          {edit ? "Save" : "Create"}
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function AllocationsDialog({ network, onClose }: { network: Network; onClose: () => void }) {
  const allocs = useNetworkAllocations(network.id);
  const cols: GridColDef[] = [
    { field: "ip", headerName: "Address", flex: 1, minWidth: 160 },
    { field: "family", headerName: "Family", width: 90, valueGetter: (v) => `IPv${v}` },
    { field: "mac", headerName: "MAC", flex: 1, minWidth: 150 },
    {
      field: "vm_id",
      headerName: "VM",
      flex: 1,
      minWidth: 160,
      valueGetter: (v) => (v ? String(v).slice(0, 8) : "reserved"),
    },
  ];
  return (
    <Dialog open onClose={onClose} maxWidth="md" fullWidth>
      <DialogTitle>IP allocations — {network.name}</DialogTitle>
      <DialogContent>
        {allocs.isError && <Alert severity="error">{(allocs.error as Error).message}</Alert>}
        <DataGrid
          autoHeight
          rows={allocs.data ?? []}
          columns={cols}
          loading={allocs.isLoading}
          getRowId={(r) => r.id}
          density="compact"
          disableRowSelectionOnClick
        />
        {allocs.data?.length === 0 && (
          <Typography variant="body2" color="text.secondary" sx={{ mt: 1 }}>
            No addresses assigned yet.
          </Typography>
        )}
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Close</Button>
      </DialogActions>
    </Dialog>
  );
}

function subnetLabel(n: Network): string {
  const parts = [n.cidr_v4, n.cidr_v6].filter(Boolean) as string[];
  return parts.length ? parts.join("  ") : "DHCP";
}

export function Networks() {
  const networks = useNetworks();
  const del = useDeleteNetwork();
  const { can } = usePermissions();
  const [dialog, setDialog] = useState<{ edit: Network | null } | null>(null);
  const [allocFor, setAllocFor] = useState<Network | null>(null);

  const columns: GridColDef<Network>[] = [
    { field: "name", headerName: "Name", flex: 1, minWidth: 140 },
    {
      field: "vlan",
      headerName: "VLAN",
      width: 90,
      valueGetter: (v) => (v == null ? "flat" : v),
    },
    {
      field: "subnet",
      headerName: "Subnet(s)",
      flex: 1.4,
      minWidth: 200,
      valueGetter: (_v, row) => subnetLabel(row),
      renderCell: (p) => (
        <Chip
          size="small"
          label={subnetLabel(p.row)}
          color={p.row.cidr_v4 || p.row.cidr_v6 ? "primary" : "default"}
          variant="outlined"
        />
      ),
    },
    {
      field: "created_at",
      headerName: "Created",
      width: 180,
      valueGetter: (v) => formatDate(v as string),
    },
    {
      field: "actions",
      type: "actions",
      headerName: "",
      width: 120,
      getActions: (p) => {
        const items = [
          <GridActionsCellItem
            key="ips"
            icon={<LanIcon />}
            label="View IPs"
            onClick={() => setAllocFor(p.row)}
          />,
        ];
        if (can("network:update")) {
          items.push(
            <GridActionsCellItem
              key="edit"
              icon={<EditIcon />}
              label="Edit"
              onClick={() => setDialog({ edit: p.row })}
            />,
          );
        }
        if (can("network:delete")) {
          items.push(
            <GridActionsCellItem
              key="del"
              icon={<DeleteIcon />}
              label="Delete"
              onClick={() => del.mutate(p.row.id)}
            />,
          );
        }
        return items;
      },
    },
  ];

  return (
    <Stack spacing={2}>
      <Stack direction="row" alignItems="center" justifyContent="space-between">
        <Typography variant="h5">Networks</Typography>
        {can("network:create") && (
          <Button variant="contained" startIcon={<AddIcon />} onClick={() => setDialog({ edit: null })}>
            Create network
          </Button>
        )}
      </Stack>
      {(networks.isError || del.error) && (
        <Alert severity="error">{((networks.error || del.error) as Error).message}</Alert>
      )}
      <div style={{ height: 480, width: "100%" }}>
        <DataGrid
          rows={networks.data ?? []}
          columns={columns}
          loading={networks.isLoading}
          density="compact"
          disableRowSelectionOnClick
        />
      </div>
      {dialog && <EditDialog edit={dialog.edit} onClose={() => setDialog(null)} />}
      {allocFor && <AllocationsDialog network={allocFor} onClose={() => setAllocFor(null)} />}
    </Stack>
  );
}
