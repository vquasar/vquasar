import { useState } from "react";
import { useNavigate } from "react-router-dom";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Checkbox from "@mui/material/Checkbox";
import Divider from "@mui/material/Divider";
import FormControlLabel from "@mui/material/FormControlLabel";
import IconButton from "@mui/material/IconButton";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import { useCreateNetwork, useCreateVm, useNetworks, useSecurityGroups } from "../api/hooks";
import type { BootSpec, CreateVmRequest, DiskSpec } from "../api/types";

type BootKind = "direct_kernel" | "firmware";

interface DiskRow {
  path: string;
  readonly: boolean;
}

export function CreateVm() {
  const navigate = useNavigate();
  const createVm = useCreateVm();
  const networks = useNetworks();
  const createNetwork = useCreateNetwork();
  const securityGroups = useSecurityGroups();

  const [name, setName] = useState("");
  const [vcpus, setVcpus] = useState(2);
  const [memoryMib, setMemoryMib] = useState(2048);
  const [bootKind, setBootKind] = useState<BootKind>("direct_kernel");
  const [kernel, setKernel] = useState("/var/lib/ch-orchestrator/images/vmlinuz");
  const [initramfs, setInitramfs] = useState("/var/lib/ch-orchestrator/images/initrd.img");
  const [cmdline, setCmdline] = useState("root=/dev/vda1 rw console=ttyS0");
  const [firmware, setFirmware] = useState("/var/lib/ch-orchestrator/firmware/CLOUDHV.fd");
  const [disks, setDisks] = useState<DiskRow[]>([{ path: "", readonly: false }]);
  const [networkId, setNetworkId] = useState<string>("");
  const [sgIds, setSgIds] = useState<string[]>([]);
  const [cloudInit, setCloudInit] = useState("");

  const setDisk = (i: number, patch: Partial<DiskRow>) =>
    setDisks((d) => d.map((row, idx) => (idx === i ? { ...row, ...patch } : row)));

  const submit = () => {
    const boot: BootSpec =
      bootKind === "direct_kernel"
        ? { type: "direct_kernel", kernel, initramfs: initramfs || null, cmdline: cmdline || null }
        : { type: "firmware", firmware };

    const diskSpecs: DiskSpec[] = disks
      .filter((d) => d.path.trim() !== "")
      .map((d) => ({ path: d.path.trim(), readonly: d.readonly, image_type: "raw" }));

    const body: CreateVmRequest = {
      name,
      spec: {
        desired_power_state: "Running",
        cpu: { boot_vcpus: vcpus, max_vcpus: vcpus },
        memory: { size_mib: memoryMib },
        boot,
        disks: diskSpecs,
        network_interfaces: networkId
          ? [{ network_id: networkId, ...(sgIds.length ? { security_groups: sgIds } : {}) }]
          : [],
        placement: {},
        cloud_init: cloudInit.trim() ? { user_data: cloudInit } : null,
      },
    };

    createVm.mutate(body, { onSuccess: () => navigate("/vms") });
  };

  return (
    <Stack spacing={2} sx={{ maxWidth: 760 }}>
      <Typography variant="h5">Create Virtual Machine</Typography>
      <Card>
        <CardContent>
          <Stack spacing={2}>
            <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} required />
            <Stack direction="row" spacing={2}>
              <TextField
                label="vCPUs"
                type="number"
                value={vcpus}
                onChange={(e) => setVcpus(Math.max(1, Number(e.target.value)))}
                sx={{ width: 140 }}
              />
              <TextField
                label="Memory (MiB)"
                type="number"
                value={memoryMib}
                onChange={(e) => setMemoryMib(Math.max(64, Number(e.target.value)))}
                sx={{ width: 180 }}
              />
            </Stack>

            <Divider textAlign="left">Boot</Divider>
            <TextField
              select
              label="Boot method"
              value={bootKind}
              onChange={(e) => setBootKind(e.target.value as BootKind)}
              sx={{ width: 260 }}
            >
              <MenuItem value="direct_kernel">Direct kernel</MenuItem>
              <MenuItem value="firmware">Firmware (CLOUDHV.fd)</MenuItem>
            </TextField>
            {bootKind === "direct_kernel" ? (
              <>
                <TextField label="Kernel path" value={kernel} onChange={(e) => setKernel(e.target.value)} />
                <TextField
                  label="Initramfs path (optional)"
                  value={initramfs}
                  onChange={(e) => setInitramfs(e.target.value)}
                />
                <TextField label="Kernel cmdline" value={cmdline} onChange={(e) => setCmdline(e.target.value)} />
              </>
            ) : (
              <TextField label="Firmware path" value={firmware} onChange={(e) => setFirmware(e.target.value)} />
            )}

            <Divider textAlign="left">Disks</Divider>
            {disks.map((d, i) => (
              <Stack key={i} direction="row" spacing={1} alignItems="center">
                <TextField
                  label={`Disk ${i + 1} path`}
                  value={d.path}
                  onChange={(e) => setDisk(i, { path: e.target.value })}
                  fullWidth
                />
                <FormControlLabel
                  control={
                    <Checkbox checked={d.readonly} onChange={(e) => setDisk(i, { readonly: e.target.checked })} />
                  }
                  label="RO"
                />
                <IconButton
                  onClick={() => setDisks((ds) => ds.filter((_, idx) => idx !== i))}
                  disabled={disks.length === 1}
                >
                  <DeleteIcon />
                </IconButton>
              </Stack>
            ))}
            <Box>
              <Button startIcon={<AddIcon />} onClick={() => setDisks((d) => [...d, { path: "", readonly: false }])}>
                Add disk
              </Button>
            </Box>

            <Divider textAlign="left">Network</Divider>
            <Stack direction="row" spacing={1} alignItems="center">
              <TextField
                select
                label="Network (optional)"
                value={networkId}
                onChange={(e) => setNetworkId(e.target.value)}
                sx={{ minWidth: 260 }}
              >
                <MenuItem value="">None</MenuItem>
                {(networks.data ?? []).map((n) => (
                  <MenuItem key={n.id} value={n.id}>
                    {n.name}
                    {n.vlan != null ? ` (VLAN ${n.vlan})` : ""}
                  </MenuItem>
                ))}
              </TextField>
              <Button
                onClick={() => {
                  const nm = prompt("New network name");
                  if (nm) createNetwork.mutate({ name: nm });
                }}
              >
                New network
              </Button>
            </Stack>
            {networkId && (
              <TextField
                select
                label="Security groups (optional)"
                value={sgIds}
                onChange={(e) =>
                  setSgIds(typeof e.target.value === "string" ? [e.target.value] : (e.target.value as string[]))
                }
                SelectProps={{ multiple: true }}
                helperText="Leave empty for an unfiltered NIC"
                sx={{ minWidth: 260, maxWidth: 420 }}
              >
                {(securityGroups.data ?? []).map((g) => (
                  <MenuItem key={g.id} value={g.id}>
                    {g.name}
                  </MenuItem>
                ))}
              </TextField>
            )}

            <Divider textAlign="left">Cloud-init</Divider>
            <TextField
              label="User-data (#cloud-config, optional)"
              value={cloudInit}
              onChange={(e) => setCloudInit(e.target.value)}
              multiline
              minRows={4}
              placeholder={"#cloud-config\nhostname: my-vm\npackages:\n  - nginx"}
              helperText="Raw NoCloud user-data, used verbatim (replaces the generated defaults)"
              slotProps={{ input: { sx: { fontFamily: "monospace", fontSize: 13 } } }}
            />

            {createVm.isError && <Alert severity="error">{(createVm.error as Error).message}</Alert>}

            <Stack direction="row" spacing={1}>
              <Button variant="contained" onClick={submit} disabled={!name || createVm.isPending}>
                Create
              </Button>
              <Button onClick={() => navigate("/vms")}>Cancel</Button>
            </Stack>
          </Stack>
        </CardContent>
      </Card>
    </Stack>
  );
}
