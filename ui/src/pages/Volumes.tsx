import { useState } from "react";
import Alert from "@mui/material/Alert";
import Button from "@mui/material/Button";
import Chip from "@mui/material/Chip";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import LinkIcon from "@mui/icons-material/Link";
import LinkOffIcon from "@mui/icons-material/LinkOff";
import { DataGrid, GridActionsCellItem, type GridColDef } from "@mui/x-data-grid";
import {
  useAttachVolume,
  useCreateVolume,
  useDeleteVolume,
  useDetachVolume,
  useVms,
  useVolumes,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { formatDate } from "../format";
import type { Vm, Volume } from "../api/types";

const GIB = 1024 * 1024 * 1024;
const fmtSize = (b: number) => `${(b / GIB).toFixed(b % GIB === 0 ? 0 : 1)} GiB`;

function CreateDialog({ onClose }: { onClose: () => void }) {
  const create = useCreateVolume();
  const [name, setName] = useState("");
  const [gib, setGib] = useState("10");
  const [format, setFormat] = useState("qcow2");
  const submit = () =>
    create.mutate(
      { name, size_bytes: Math.round(Number(gib) * GIB), format },
      { onSuccess: onClose },
    );
  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Create volume</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          <TextField
            label="Size (GiB)"
            type="number"
            value={gib}
            onChange={(e) => setGib(e.target.value)}
          />
          <TextField select label="Format" value={format} onChange={(e) => setFormat(e.target.value)}>
            <MenuItem value="qcow2">qcow2 (thin)</MenuItem>
            <MenuItem value="raw">raw</MenuItem>
          </TextField>
          {create.error && <Alert severity="error">{(create.error as Error).message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button
          variant="contained"
          onClick={submit}
          disabled={!name || !(Number(gib) > 0) || create.isPending}
        >
          Create
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function AttachDialog({ volume, vms, onClose }: { volume: Volume; vms: Vm[]; onClose: () => void }) {
  const attach = useAttachVolume();
  const [vmId, setVmId] = useState("");
  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Attach {volume.name}</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField select label="VM" value={vmId} onChange={(e) => setVmId(e.target.value)}>
            {vms.map((v) => (
              <MenuItem key={v.id} value={v.id}>
                {v.name} ({v.phase})
              </MenuItem>
            ))}
          </TextField>
          <Typography variant="caption" color="text.secondary">
            Hot-adds to a running VM; otherwise attaches on next start.
          </Typography>
          {attach.error && <Alert severity="error">{(attach.error as Error).message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button
          variant="contained"
          disabled={!vmId || attach.isPending}
          onClick={() => attach.mutate({ id: volume.id, vmId }, { onSuccess: onClose })}
        >
          Attach
        </Button>
      </DialogActions>
    </Dialog>
  );
}

export function Volumes() {
  const volumes = useVolumes();
  const vms = useVms();
  const del = useDeleteVolume();
  const detach = useDetachVolume();
  const { can } = usePermissions();
  const [creating, setCreating] = useState(false);
  const [attachVol, setAttachVol] = useState<Volume | null>(null);

  const vmName = (id: string | null) =>
    id ? (vms.data?.find((v) => v.id === id)?.name ?? id.slice(0, 8)) : null;

  const cols: GridColDef<Volume>[] = [
    { field: "name", headerName: "Name", flex: 1, minWidth: 140 },
    { field: "size_bytes", headerName: "Size", width: 110, valueGetter: (v) => fmtSize(v as number) },
    { field: "format", headerName: "Format", width: 90 },
    {
      field: "attached_vm_id",
      headerName: "Attached to",
      flex: 1,
      minWidth: 160,
      renderCell: (p) =>
        p.row.attached_vm_id ? (
          <Chip size="small" color="primary" label={vmName(p.row.attached_vm_id)} />
        ) : (
          <Chip size="small" label="available" variant="outlined" />
        ),
    },
    {
      field: "created_at",
      headerName: "Created",
      width: 170,
      valueGetter: (v) => formatDate(v as string),
    },
    {
      field: "actions",
      type: "actions",
      headerName: "",
      width: 100,
      getActions: (p) => {
        const items = [];
        if (can("volume:update")) {
          if (p.row.attached_vm_id) {
            items.push(
              <GridActionsCellItem
                key="detach"
                icon={<LinkOffIcon />}
                label="Detach"
                onClick={() => detach.mutate(p.row.id)}
              />,
            );
          } else {
            items.push(
              <GridActionsCellItem
                key="attach"
                icon={<LinkIcon />}
                label="Attach"
                onClick={() => setAttachVol(p.row)}
              />,
            );
          }
        }
        if (can("volume:delete") && !p.row.attached_vm_id) {
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
        <Typography variant="h5">Volumes</Typography>
        {can("volume:create") && (
          <Button variant="contained" startIcon={<AddIcon />} onClick={() => setCreating(true)}>
            Create volume
          </Button>
        )}
      </Stack>
      {(volumes.error || del.error || detach.error) && (
        <Alert severity="error">
          {((volumes.error || del.error || detach.error) as Error).message}
        </Alert>
      )}
      <div style={{ height: 520, width: "100%" }}>
        <DataGrid
          rows={volumes.data ?? []}
          columns={cols}
          loading={volumes.isLoading}
          density="compact"
          disableRowSelectionOnClick
        />
      </div>
      {creating && <CreateDialog onClose={() => setCreating(false)} />}
      {attachVol && (
        <AttachDialog volume={attachVol} vms={vms.data ?? []} onClose={() => setAttachVol(null)} />
      )}
    </Stack>
  );
}
