import { useState } from "react";
import Alert from "@mui/material/Alert";
import Button from "@mui/material/Button";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import FormControlLabel from "@mui/material/FormControlLabel";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import { DataGrid, GridActionsCellItem, type GridColDef } from "@mui/x-data-grid";
import { useCreateImage, useDeleteImage, useImages } from "../api/hooks";
import { formatBytes, formatDate } from "../format";
import type { BootSpec, CreateImageRequest, Image } from "../api/types";

const GIB = 1024 * 1024 * 1024;

function CreateDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [name, setName] = useState("");
  const [os, setOs] = useState("");
  const [sourcePath, setSourcePath] = useState("");
  const [format, setFormat] = useState<"raw" | "qcow2">("raw");
  const [bootType, setBootType] = useState<"direct_kernel" | "firmware">("direct_kernel");
  const [kernel, setKernel] = useState("");
  const [initramfs, setInitramfs] = useState("");
  const [cmdline, setCmdline] = useState("root=/dev/vda1 rw console=ttyS0");
  const [firmware, setFirmware] = useState("");
  const [sizeGib, setSizeGib] = useState("");
  const [cloudInit, setCloudInit] = useState(true);
  const create = useCreateImage();

  const submit = () => {
    const boot: BootSpec =
      bootType === "direct_kernel"
        ? {
            type: "direct_kernel",
            kernel,
            initramfs: initramfs || null,
            cmdline: cmdline || null,
          }
        : { type: "firmware", firmware };
    const body: CreateImageRequest = {
      name,
      source_path: sourcePath,
      format,
      boot,
      default_size_bytes: sizeGib ? Math.round(Number(sizeGib) * GIB) : null,
      cloud_init: cloudInit,
      os: os || null,
    };
    create.mutate(body, { onSuccess: onClose });
  };

  const ready = name && sourcePath && (bootType === "firmware" ? firmware : kernel);

  return (
    <Dialog open={open} onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Register image</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          <TextField label="OS label (optional)" value={os} onChange={(e) => setOs(e.target.value)} />
          <TextField
            label="Base disk path (shared storage)"
            value={sourcePath}
            onChange={(e) => setSourcePath(e.target.value)}
            helperText="e.g. /var/lib/ch-orchestrator/shared/images/ubuntu-26.04.raw"
          />
          <TextField select label="Base format" value={format} onChange={(e) => setFormat(e.target.value as "raw" | "qcow2")}>
            <MenuItem value="raw">raw</MenuItem>
            <MenuItem value="qcow2">qcow2</MenuItem>
          </TextField>
          <TextField select label="Boot" value={bootType} onChange={(e) => setBootType(e.target.value as "direct_kernel" | "firmware")}>
            <MenuItem value="direct_kernel">Direct kernel</MenuItem>
            <MenuItem value="firmware">Firmware (UEFI)</MenuItem>
          </TextField>
          {bootType === "direct_kernel" ? (
            <>
              <TextField label="Kernel path" value={kernel} onChange={(e) => setKernel(e.target.value)} />
              <TextField label="Initramfs path (optional)" value={initramfs} onChange={(e) => setInitramfs(e.target.value)} />
              <TextField label="Kernel cmdline" value={cmdline} onChange={(e) => setCmdline(e.target.value)} />
            </>
          ) : (
            <TextField label="Firmware path (CLOUDHV.fd)" value={firmware} onChange={(e) => setFirmware(e.target.value)} />
          )}
          <TextField
            label="Default disk size (GiB, optional)"
            value={sizeGib}
            onChange={(e) => setSizeGib(e.target.value)}
            helperText="Grow provisioned volumes to this size"
          />
          <FormControlLabel
            control={<Switch checked={cloudInit} onChange={(e) => setCloudInit(e.target.checked)} />}
            label="Expects cloud-init seed"
          />
          {create.isError && <Alert severity="error">{(create.error as Error).message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="contained" onClick={submit} disabled={!ready || create.isPending}>
          Register
        </Button>
      </DialogActions>
    </Dialog>
  );
}

export function Images() {
  const images = useImages();
  const del = useDeleteImage();
  const [dialog, setDialog] = useState(false);

  const columns: GridColDef<Image>[] = [
    { field: "name", headerName: "Name", flex: 1, minWidth: 160 },
    { field: "os", headerName: "OS", width: 140, valueGetter: (v) => v ?? "—" },
    { field: "format", headerName: "Format", width: 90 },
    { field: "source_path", headerName: "Base disk", flex: 1, minWidth: 240 },
    {
      field: "default_size_bytes",
      headerName: "Default size",
      width: 120,
      valueGetter: (v) => (v == null ? "—" : formatBytes(v as number)),
    },
    {
      field: "cloud_init",
      headerName: "cloud-init",
      width: 100,
      valueGetter: (v) => (v ? "yes" : "no"),
    },
    { field: "created_at", headerName: "Created", width: 190, valueGetter: (v) => formatDate(v as string) },
    {
      field: "actions",
      type: "actions",
      headerName: "",
      width: 60,
      getActions: (p) => [
        <GridActionsCellItem key="del" icon={<DeleteIcon />} label="Delete" onClick={() => del.mutate(p.row.id)} />,
      ],
    },
  ];

  return (
    <Stack spacing={2}>
      <Stack direction="row" alignItems="center" justifyContent="space-between">
        <Typography variant="h5">Images</Typography>
        <Button variant="contained" startIcon={<AddIcon />} onClick={() => setDialog(true)}>
          Register image
        </Button>
      </Stack>
      {images.isError && <Alert severity="error">{(images.error as Error).message}</Alert>}
      {del.isError && <Alert severity="error">{(del.error as Error).message}</Alert>}
      <div style={{ height: 480, width: "100%" }}>
        <DataGrid
          rows={images.data ?? []}
          columns={columns}
          loading={images.isLoading}
          density="compact"
          disableRowSelectionOnClick
        />
      </div>
      <CreateDialog open={dialog} onClose={() => setDialog(false)} />
    </Stack>
  );
}
