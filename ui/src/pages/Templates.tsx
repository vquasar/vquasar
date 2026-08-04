import { useState } from "react";
import { useNavigate } from "react-router-dom";
import Alert from "@mui/material/Alert";
import Button from "@mui/material/Button";
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
import EditIcon from "@mui/icons-material/Edit";
import RocketLaunchIcon from "@mui/icons-material/RocketLaunch";
import { DataGrid, GridActionsCellItem, type GridColDef } from "@mui/x-data-grid";
import {
  useCreateTemplate,
  useCreateVmFromTemplate,
  useDeleteTemplate,
  useImages,
  useNetworks,
  useTemplates,
  useUpdateTemplate,
} from "../api/hooks";
import { formatBytes, formatDate, formatMib } from "../format";
import type { CreateTemplateRequest, Template } from "../api/types";
import { usePermissions } from "../auth/permissions";

const GIB = 1024 * 1024 * 1024;

function EditDialog({ edit, onClose }: { edit: Template | null; onClose: () => void }) {
  const images = useImages();
  const networks = useNetworks();
  const create = useCreateTemplate();
  const update = useUpdateTemplate();
  const [name, setName] = useState(edit?.name ?? "");
  const [imageId, setImageId] = useState(edit?.image_id ?? "");
  const [vcpus, setVcpus] = useState(String(edit?.boot_vcpus ?? 2));
  const [memMib, setMemMib] = useState(String(edit?.memory_mib ?? 2048));
  const [sizeGib, setSizeGib] = useState(
    edit?.disk_size_bytes ? String(Math.round(edit.disk_size_bytes / GIB)) : "10",
  );
  const [format, setFormat] = useState<"qcow2" | "raw">(edit?.disk_format ?? "qcow2");
  const [networkId, setNetworkId] = useState(edit?.network_id ?? "");
  const [password, setPassword] = useState(edit?.cloud_init?.password ?? "");
  const [sshKey, setSshKey] = useState(edit?.cloud_init?.ssh_authorized_keys?.[0] ?? "");

  const submit = () => {
    const body: CreateTemplateRequest = {
      name,
      image_id: imageId,
      boot_vcpus: Number(vcpus),
      max_vcpus: Number(vcpus),
      memory_mib: Number(memMib),
      disk_size_bytes: sizeGib ? Math.round(Number(sizeGib) * GIB) : null,
      disk_format: format,
      network_id: networkId || null,
      cloud_init:
        password || sshKey
          ? {
              password: password || null,
              ssh_authorized_keys: sshKey ? [sshKey] : [],
            }
          : null,
    };
    if (edit) update.mutate({ id: edit.id, body }, { onSuccess: onClose });
    else create.mutate(body, { onSuccess: onClose });
  };

  const busy = create.isPending || update.isPending;
  const err = (create.error || update.error) as Error | null;

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>{edit ? "Edit template" : "Create template"}</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          <TextField select label="Image" value={imageId} onChange={(e) => setImageId(e.target.value)}>
            {(images.data ?? []).map((img) => (
              <MenuItem key={img.id} value={img.id}>
                {img.name}
              </MenuItem>
            ))}
          </TextField>
          <Stack direction="row" spacing={2}>
            <TextField label="vCPUs" value={vcpus} onChange={(e) => setVcpus(e.target.value)} fullWidth />
            <TextField label="Memory (MiB)" value={memMib} onChange={(e) => setMemMib(e.target.value)} fullWidth />
          </Stack>
          <Stack direction="row" spacing={2}>
            <TextField label="Disk size (GiB)" value={sizeGib} onChange={(e) => setSizeGib(e.target.value)} fullWidth />
            <TextField select label="Disk format" value={format} onChange={(e) => setFormat(e.target.value as "qcow2" | "raw")} fullWidth>
              <MenuItem value="qcow2">qcow2 (thin overlay)</MenuItem>
              <MenuItem value="raw">raw (full copy)</MenuItem>
            </TextField>
          </Stack>
          <TextField select label="Network (optional)" value={networkId} onChange={(e) => setNetworkId(e.target.value)}>
            <MenuItem value="">— none —</MenuItem>
            {(networks.data ?? []).map((n) => (
              <MenuItem key={n.id} value={n.id}>
                {n.name}
              </MenuItem>
            ))}
          </TextField>
          <TextField label="Default password (optional)" value={password} onChange={(e) => setPassword(e.target.value)} />
          <TextField label="Default SSH public key (optional)" value={sshKey} onChange={(e) => setSshKey(e.target.value)} />
          {err && <Alert severity="error">{err.message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="contained" onClick={submit} disabled={!name || !imageId || busy}>
          {edit ? "Save" : "Create"}
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function LaunchDialog({ template, onClose }: { template: Template | null; onClose: () => void }) {
  const launch = useCreateVmFromTemplate();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [cloudInit, setCloudInit] = useState("");

  if (!template) return null;
  const submit = () =>
    launch.mutate(
      {
        name,
        template_id: template.id,
        overrides: cloudInit.trim() ? { cloud_init: { user_data: cloudInit } } : undefined,
      },
      {
        onSuccess: () => {
          onClose();
          navigate("/vms");
        },
      },
    );

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Launch VM from “{template.name}”</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField label="VM name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          <Typography variant="caption" color="text.secondary">
            {template.boot_vcpus} vCPU · {formatMib(template.memory_mib)} ·{" "}
            {template.disk_size_bytes ? formatBytes(template.disk_size_bytes) : "image default"} ·{" "}
            {template.disk_format}
          </Typography>
          <TextField
            label="Cloud-init user-data (optional)"
            value={cloudInit}
            onChange={(e) => setCloudInit(e.target.value)}
            multiline
            minRows={4}
            placeholder={"#cloud-config\nhostname: my-vm\npackages:\n  - nginx"}
            helperText="Raw NoCloud user-data, used verbatim (overrides the template's cloud-init)"
            slotProps={{ input: { sx: { fontFamily: "monospace", fontSize: 13 } } }}
          />
          {launch.isError && <Alert severity="error">{(launch.error as Error).message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="contained" onClick={submit} disabled={!name || launch.isPending}>
          Launch
        </Button>
      </DialogActions>
    </Dialog>
  );
}

export function Templates() {
  const templates = useTemplates();
  const images = useImages();
  const del = useDeleteTemplate();
  const { can } = usePermissions();
  const [dialog, setDialog] = useState<{ edit: Template | null } | null>(null);
  const [launch, setLaunch] = useState<Template | null>(null);

  const imageName = (id: string) => images.data?.find((i) => i.id === id)?.name ?? id.slice(0, 8);

  const columns: GridColDef<Template>[] = [
    { field: "name", headerName: "Name", flex: 1, minWidth: 160 },
    { field: "image_id", headerName: "Image", width: 160, valueGetter: (v) => imageName(v as string) },
    { field: "boot_vcpus", headerName: "vCPU", width: 80 },
    { field: "memory_mib", headerName: "Memory", width: 110, valueGetter: (v) => formatMib(v as number) },
    {
      field: "disk_size_bytes",
      headerName: "Disk",
      width: 110,
      valueGetter: (v) => (v == null ? "image default" : formatBytes(v as number)),
    },
    { field: "disk_format", headerName: "Format", width: 90 },
    { field: "created_at", headerName: "Created", width: 180, valueGetter: (v) => formatDate(v as string) },
    {
      field: "actions",
      type: "actions",
      headerName: "",
      width: 130,
      getActions: (p) => [
        <GridActionsCellItem
          key="launch"
          icon={<RocketLaunchIcon />}
          label="Launch VM"
          onClick={() => setLaunch(p.row)}
        />,
        <GridActionsCellItem key="edit" icon={<EditIcon />} label="Edit" onClick={() => setDialog({ edit: p.row })} />,
        <GridActionsCellItem key="del" icon={<DeleteIcon />} label="Delete" onClick={() => del.mutate(p.row.id)} />,
      ],
    },
  ];

  return (
    <Stack spacing={2}>
      <Stack direction="row" alignItems="center" justifyContent="space-between">
        <Typography variant="h5">Templates</Typography>
        {can("template:create") && (
          <Button variant="contained" startIcon={<AddIcon />} onClick={() => setDialog({ edit: null })}>
            Create template
          </Button>
        )}
      </Stack>
      {templates.isError && <Alert severity="error">{(templates.error as Error).message}</Alert>}
      {del.isError && <Alert severity="error">{(del.error as Error).message}</Alert>}
      <div style={{ height: 480, width: "100%" }}>
        <DataGrid
          rows={templates.data ?? []}
          columns={columns}
          loading={templates.isLoading}
          density="compact"
          disableRowSelectionOnClick
        />
      </div>
      {dialog && <EditDialog edit={dialog.edit} onClose={() => setDialog(null)} />}
      <LaunchDialog template={launch} onClose={() => setLaunch(null)} />
    </Stack>
  );
}
