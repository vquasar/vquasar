import { useState } from "react";
import Alert from "@mui/material/Alert";
import Button from "@mui/material/Button";
import Chip from "@mui/material/Chip";
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
import EditIcon from "@mui/icons-material/Edit";
import CloudDownloadIcon from "@mui/icons-material/CloudDownload";
import UploadIcon from "@mui/icons-material/Upload";
import { DataGrid, GridActionsCellItem, type GridColDef } from "@mui/x-data-grid";
import {
  useCreateImage,
  useDeleteImage,
  useImages,
  useImportImage,
  useUpdateImage,
  useUploadImage,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { formatBytes, formatDate } from "../format";
import type { BootSpec, CreateImageRequest, Image } from "../api/types";

const GIB = 1024 * 1024 * 1024;

function EditDialog({ edit, onClose }: { edit: Image | null; onClose: () => void }) {
  const dk = edit?.boot.type === "direct_kernel" ? edit.boot : null;
  const fw = edit?.boot.type === "firmware" ? edit.boot : null;
  const [name, setName] = useState(edit?.name ?? "");
  const [os, setOs] = useState(edit?.os ?? "");
  const [sourcePath, setSourcePath] = useState(edit?.source_path ?? "");
  const [format, setFormat] = useState<"raw" | "qcow2">(edit?.format ?? "raw");
  const [bootType, setBootType] = useState<"direct_kernel" | "firmware">(
    edit?.boot.type ?? "direct_kernel",
  );
  const [kernel, setKernel] = useState(dk?.kernel ?? "");
  const [initramfs, setInitramfs] = useState(dk?.initramfs ?? "");
  const [cmdline, setCmdline] = useState(dk?.cmdline ?? "root=/dev/vda1 rw console=ttyS0");
  const [firmware, setFirmware] = useState(fw?.firmware ?? "");
  const [sizeGib, setSizeGib] = useState(
    edit?.default_size_bytes ? String(Math.round(edit.default_size_bytes / GIB)) : "",
  );
  const [cloudInit, setCloudInit] = useState(edit?.cloud_init ?? true);
  const create = useCreateImage();
  const update = useUpdateImage();

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
    if (edit) update.mutate({ id: edit.id, body }, { onSuccess: onClose });
    else create.mutate(body, { onSuccess: onClose });
  };

  const busy = create.isPending || update.isPending;
  const err = (create.error || update.error) as Error | null;
  const ready = name && sourcePath && (bootType === "firmware" ? firmware : kernel);

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>{edit ? "Edit image" : "Register image"}</DialogTitle>
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
          {err && <Alert severity="error">{err.message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="contained" onClick={submit} disabled={!ready || busy}>
          {edit ? "Save" : "Register"}
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function ImportDialog({ onClose }: { onClose: () => void }) {
  const imp = useImportImage();
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [format, setFormat] = useState<"raw" | "qcow2">("qcow2");
  const [os, setOs] = useState("");
  const [firmware, setFirmware] = useState("/var/lib/ch-orchestrator/firmware/CLOUDHV.fd");
  const [sizeGib, setSizeGib] = useState("");
  const [cloudInit, setCloudInit] = useState(true);

  const submit = () =>
    imp.mutate(
      {
        name,
        url,
        format,
        os: os || null,
        cloud_init: cloudInit,
        boot: { type: "firmware", firmware },
        default_size_bytes: sizeGib ? Math.round(Number(sizeGib) * GIB) : null,
      },
      { onSuccess: onClose },
    );

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Import image from URL</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          <TextField
            label="URL"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="https://cloud-images.ubuntu.com/…/disk.img"
          />
          <Stack direction="row" spacing={2}>
            <TextField select label="Format" value={format} onChange={(e) => setFormat(e.target.value as "raw" | "qcow2")} sx={{ minWidth: 120 }}>
              <MenuItem value="qcow2">qcow2</MenuItem>
              <MenuItem value="raw">raw</MenuItem>
            </TextField>
            <TextField label="OS label" value={os} onChange={(e) => setOs(e.target.value)} fullWidth placeholder="ubuntu-26.04" />
          </Stack>
          <TextField label="Firmware (UEFI) path" value={firmware} onChange={(e) => setFirmware(e.target.value)} />
          <TextField label="Default size (GiB, optional)" value={sizeGib} onChange={(e) => setSizeGib(e.target.value)} />
          <FormControlLabel
            control={<Switch checked={cloudInit} onChange={(e) => setCloudInit(e.target.checked)} />}
            label="Uses cloud-init"
          />
          <Typography variant="caption" color="text.secondary">
            The download runs in the background; the image becomes usable when it turns “ready”.
          </Typography>
          {imp.error && <Alert severity="error">{(imp.error as Error).message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="contained" onClick={submit} disabled={!name || !url || imp.isPending}>
          Import
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function UploadDialog({ onClose }: { onClose: () => void }) {
  const up = useUploadImage();
  const [name, setName] = useState("");
  const [format, setFormat] = useState<"raw" | "qcow2">("qcow2");
  const [os, setOs] = useState("");
  const [firmware, setFirmware] = useState("/var/lib/ch-orchestrator/firmware/CLOUDHV.fd");
  const [file, setFile] = useState<File | null>(null);

  const submit = () => {
    if (!file) return;
    up.mutate(
      { params: { name, format, os, firmware, cloud_init: "true" }, file },
      { onSuccess: onClose },
    );
  };
  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Upload image</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <Button variant="outlined" component="label">
            {file ? file.name : "Choose disk file…"}
            <input
              type="file"
              hidden
              onChange={(e) => {
                const f = e.target.files?.[0] ?? null;
                setFile(f);
                if (f && !name) setName(f.name.replace(/\.(qcow2|img|raw)$/i, ""));
              }}
            />
          </Button>
          <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} />
          <Stack direction="row" spacing={2}>
            <TextField select label="Format" value={format} onChange={(e) => setFormat(e.target.value as "raw" | "qcow2")} sx={{ minWidth: 120 }}>
              <MenuItem value="qcow2">qcow2</MenuItem>
              <MenuItem value="raw">raw</MenuItem>
            </TextField>
            <TextField label="OS label" value={os} onChange={(e) => setOs(e.target.value)} fullWidth />
          </Stack>
          <TextField label="Firmware (UEFI) path" value={firmware} onChange={(e) => setFirmware(e.target.value)} />
          {up.isPending && <Alert severity="info">Uploading… keep this dialog open.</Alert>}
          {up.error && <Alert severity="error">{(up.error as Error).message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="contained" onClick={submit} disabled={!name || !file || up.isPending}>
          Upload
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function StatusChip({ image }: { image: Image }) {
  const color = image.status === "ready" ? "success" : image.status === "failed" ? "error" : "warning";
  return <Chip size="small" color={color} label={image.status} title={image.error ?? undefined} />;
}

export function Images() {
  const images = useImages();
  const del = useDeleteImage();
  const { can } = usePermissions();
  const [dialog, setDialog] = useState<{ edit: Image | null } | null>(null);
  const [importing, setImporting] = useState(false);
  const [uploading, setUploading] = useState(false);

  const columns: GridColDef<Image>[] = [
    { field: "name", headerName: "Name", flex: 1, minWidth: 160 },
    {
      field: "status",
      headerName: "Status",
      width: 110,
      renderCell: (p) => <StatusChip image={p.row} />,
    },
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
      width: 90,
      getActions: (p) => [
        <GridActionsCellItem key="edit" icon={<EditIcon />} label="Edit" onClick={() => setDialog({ edit: p.row })} />,
        <GridActionsCellItem key="del" icon={<DeleteIcon />} label="Delete" onClick={() => del.mutate(p.row.id)} />,
      ],
    },
  ];

  return (
    <Stack spacing={2}>
      <Stack direction="row" alignItems="center" justifyContent="space-between">
        <Typography variant="h5">Images</Typography>
        {can("image:create") && (
          <Stack direction="row" spacing={1}>
            <Button startIcon={<UploadIcon />} onClick={() => setUploading(true)}>
              Upload
            </Button>
            <Button startIcon={<CloudDownloadIcon />} onClick={() => setImporting(true)}>
              Import from URL
            </Button>
            <Button variant="contained" startIcon={<AddIcon />} onClick={() => setDialog({ edit: null })}>
              Register image
            </Button>
          </Stack>
        )}
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
      {dialog && <EditDialog edit={dialog.edit} onClose={() => setDialog(null)} />}
      {importing && <ImportDialog onClose={() => setImporting(false)} />}
      {uploading && <UploadDialog onClose={() => setUploading(false)} />}
    </Stack>
  );
}
