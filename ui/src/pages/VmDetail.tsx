import { useState } from "react";
import { Link as RouterLink, useNavigate, useParams } from "react-router-dom";
import Alert from "@mui/material/Alert";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import Grid from "@mui/material/Grid2";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import Table from "@mui/material/Table";
import TableBody from "@mui/material/TableBody";
import TableCell from "@mui/material/TableCell";
import TableRow from "@mui/material/TableRow";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import Divider from "@mui/material/Divider";
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import StopIcon from "@mui/icons-material/Stop";
import DeleteIcon from "@mui/icons-material/Delete";
import TerminalIcon from "@mui/icons-material/Terminal";
import SwapHorizIcon from "@mui/icons-material/SwapHoriz";
import EditIcon from "@mui/icons-material/Edit";
import {
  useChangeNic,
  useHosts,
  useMigrateVm,
  useNetworks,
  useSecurityGroups,
  useUpdateVm,
  useVm,
  useVmAction,
  useVmMetrics,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { StatusChip } from "../components/StatusChip";
import { formatBytes, formatDate, formatMib, shortId } from "../format";
import type { UpdateVmRequest, Vm } from "../api/types";

const GIB = 1024 * 1024 * 1024;

function EditVmDialog({ vm, onClose }: { vm: Vm; onClose: () => void }) {
  const update = useUpdateVm();
  const networks = useNetworks();
  const [name, setName] = useState(vm.name);
  const [bootVcpus, setBootVcpus] = useState(String(vm.spec.cpu.boot_vcpus));
  const [maxVcpus, setMaxVcpus] = useState(String(vm.spec.cpu.max_vcpus));
  const [memMib, setMemMib] = useState(String(vm.spec.memory.size_mib));
  const [maxMemMib, setMaxMemMib] = useState(
    vm.spec.memory.max_size_mib ? String(vm.spec.memory.max_size_mib) : "",
  );
  const [growIdx, setGrowIdx] = useState("");
  const [growGib, setGrowGib] = useState("");
  const [addDiskGib, setAddDiskGib] = useState("");
  const [addNic, setAddNic] = useState("");

  const writableDisks = vm.spec.disks
    .map((d, i) => ({ d, i }))
    .filter(({ d }) => !d.readonly);

  const submit = () => {
    const body: UpdateVmRequest = {};
    if (name !== vm.name) body.name = name;
    if (Number(bootVcpus) !== vm.spec.cpu.boot_vcpus) body.boot_vcpus = Number(bootVcpus);
    if (Number(maxVcpus) !== vm.spec.cpu.max_vcpus) body.max_vcpus = Number(maxVcpus);
    if (Number(memMib) !== vm.spec.memory.size_mib) body.memory_mib = Number(memMib);
    if (maxMemMib && Number(maxMemMib) !== (vm.spec.memory.max_size_mib ?? 0))
      body.memory_max_mib = Number(maxMemMib);
    if (growIdx !== "" && growGib)
      body.grow_disk = { index: Number(growIdx), size_bytes: Math.round(Number(growGib) * GIB) };
    if (addDiskGib)
      body.add_disk = { size_bytes: Math.round(Number(addDiskGib) * GIB), image_type: "qcow2" };
    if (addNic) body.add_nic = { network_id: addNic };
    update.mutate({ id: vm.id, body }, { onSuccess: onClose });
  };

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Edit “{vm.name}”</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} />
          <Stack direction="row" spacing={2}>
            <TextField
              label="vCPUs"
              value={bootVcpus}
              onChange={(e) => setBootVcpus(e.target.value)}
              helperText={`hot-plug up to max ${maxVcpus}`}
              fullWidth
            />
            <TextField
              label="Max vCPUs"
              value={maxVcpus}
              onChange={(e) => setMaxVcpus(e.target.value)}
              helperText="raising needs restart"
              fullWidth
            />
          </Stack>
          <Stack direction="row" spacing={2}>
            <TextField
              label="Memory (MiB)"
              value={memMib}
              onChange={(e) => setMemMib(e.target.value)}
              helperText={maxMemMib ? `hot-plug up to ${maxMemMib}` : "restart to change"}
              fullWidth
            />
            <TextField
              label="Max memory (MiB)"
              value={maxMemMib}
              onChange={(e) => setMaxMemMib(e.target.value)}
              helperText="enables live resize; needs restart"
              fullWidth
            />
          </Stack>
          <Divider>Disks</Divider>
          <Stack direction="row" spacing={2}>
            <TextField
              select
              label="Grow disk"
              value={growIdx}
              onChange={(e) => setGrowIdx(e.target.value)}
              fullWidth
            >
              <MenuItem value="">— none —</MenuItem>
              {writableDisks.map(({ d, i }) => (
                <MenuItem key={i} value={String(i)}>
                  {d.path.split("/").pop()}
                  {d.size_bytes ? ` (${formatBytes(d.size_bytes)})` : ""}
                </MenuItem>
              ))}
            </TextField>
            <TextField
              label="New size (GiB)"
              value={growGib}
              onChange={(e) => setGrowGib(e.target.value)}
              helperText="applied on next Stop → Start (grow the guest FS after)"
              fullWidth
            />
          </Stack>
          <TextField
            label="Add data disk (GiB, optional)"
            value={addDiskGib}
            onChange={(e) => setAddDiskGib(e.target.value)}
            helperText="blank qcow2, hot-added"
          />
          <Divider>Network</Divider>
          <TextField
            select
            label="Add NIC on network (optional)"
            value={addNic}
            onChange={(e) => setAddNic(e.target.value)}
          >
            <MenuItem value="">— none —</MenuItem>
            {(networks.data ?? []).map((n) => (
              <MenuItem key={n.id} value={n.id}>
                {n.name}
              </MenuItem>
            ))}
          </TextField>
          {update.isError && <Alert severity="error">{(update.error as Error).message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="contained" onClick={submit} disabled={update.isPending}>
          Apply
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function Row({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <TableRow>
      <TableCell sx={{ color: "text.secondary", width: 180, border: 0 }}>{label}</TableCell>
      <TableCell sx={{ border: 0 }}>{value}</TableCell>
    </TableRow>
  );
}

/// A VM's NICs with a "change network" action (design M13d).
function NicList({ vmId, nics }: { vmId: string; nics: { network_id: string; security_groups?: string[] }[] }) {
  const networks = useNetworks();
  const securityGroups = useSecurityGroups();
  const change = useChangeNic();
  const [editIdx, setEditIdx] = useState<number | null>(null);
  const [networkId, setNetworkId] = useState("");
  const [sgIds, setSgIds] = useState<string[]>([]);
  const nameOf = (id: string) => networks.data?.find((n) => n.id === id)?.name ?? id.slice(0, 8);

  const open = (i: number) => {
    setEditIdx(i);
    setNetworkId(nics[i].network_id);
    setSgIds(nics[i].security_groups ?? []);
  };
  const submit = () => {
    if (editIdx == null) return;
    change.mutate(
      { id: vmId, index: editIdx, networkId, securityGroups: sgIds },
      { onSuccess: () => setEditIdx(null) },
    );
  };

  return (
    <>
      {nics.map((nic, i) => (
        <div key={i} style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span>
            eth{i}: {nameOf(nic.network_id)}
          </span>
          <Button size="small" onClick={() => open(i)}>
            Change network
          </Button>
        </div>
      ))}
      {nics.length === 0 && <span>none</span>}
      <Dialog open={editIdx != null} onClose={() => setEditIdx(null)} maxWidth="sm" fullWidth>
        <DialogTitle>Change eth{editIdx} network</DialogTitle>
        <DialogContent>
          <Stack spacing={2} sx={{ mt: 1 }}>
            <TextField
              select
              label="Network"
              value={networkId}
              onChange={(e) => setNetworkId(e.target.value)}
            >
              {(networks.data ?? []).map((n) => (
                <MenuItem key={n.id} value={n.id}>
                  {n.name}
                </MenuItem>
              ))}
            </TextField>
            <TextField
              select
              label="Security groups (optional)"
              value={sgIds}
              onChange={(e) =>
                setSgIds(typeof e.target.value === "string" ? [e.target.value] : (e.target.value as string[]))
              }
              SelectProps={{ multiple: true }}
            >
              {(securityGroups.data ?? []).map((g) => (
                <MenuItem key={g.id} value={g.id}>
                  {g.name}
                </MenuItem>
              ))}
            </TextField>
            <Typography variant="caption" color="text.secondary">
              The NIC re-homes without a restart. The guest keeps its IP, so on a different subnet
              renew DHCP or reconfigure it.
            </Typography>
            {change.error && <Alert severity="error">{(change.error as Error).message}</Alert>}
          </Stack>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setEditIdx(null)}>Cancel</Button>
          <Button variant="contained" onClick={submit} disabled={!networkId || change.isPending}>
            Change
          </Button>
        </DialogActions>
      </Dialog>
    </>
  );
}

/// Live resource usage, polled from the agent via GET /vms/:id/metrics (M15a).
/// Rendered only for a running VM; the agent reports not-running otherwise.
function MetricsCard({ vmId }: { vmId: string }) {
  const q = useVmMetrics(vmId);
  const m = q.data;
  return (
    <Card>
      <CardContent>
        <Typography variant="h6" gutterBottom>
          Live metrics
        </Typography>
        {!m || !m.running ? (
          <Typography color="text.secondary">
            {q.isLoading ? "Loading…" : "Not running — no live metrics."}
          </Typography>
        ) : (
          <Table size="small">
            <TableBody>
              <Row label="CPU" value={`${m.cpu_pct.toFixed(1)} %`} />
              <Row label="Memory (RSS)" value={formatBytes(m.mem_bytes)} />
              <Row
                label="Disk read"
                value={`${formatBytes(m.disk_read_bytes)} · ${m.disk_read_ops.toLocaleString()} ops`}
              />
              <Row
                label="Disk write"
                value={`${formatBytes(m.disk_write_bytes)} · ${m.disk_write_ops.toLocaleString()} ops`}
              />
              <Row
                label="Net RX"
                value={`${formatBytes(m.net_rx_bytes)} · ${m.net_rx_packets.toLocaleString()} pkts`}
              />
              <Row
                label="Net TX"
                value={`${formatBytes(m.net_tx_bytes)} · ${m.net_tx_packets.toLocaleString()} pkts`}
              />
            </TableBody>
          </Table>
        )}
        <Typography variant="caption" color="text.secondary">
          Disk and network are cumulative since boot; CPU is a live sample.
        </Typography>
      </CardContent>
    </Card>
  );
}

export function VmDetail() {
  const { id } = useParams();
  const navigate = useNavigate();
  const vm = useVm(id);
  const action = useVmAction();
  const hosts = useHosts();
  const migrate = useMigrateVm();
  const { can } = usePermissions();
  const [migrateOpen, setMigrateOpen] = useState(false);
  const [editOpen, setEditOpen] = useState(false);
  const [target, setTarget] = useState("");

  if (vm.isLoading) return <Typography>Loading…</Typography>;
  if (vm.isError) return <Alert severity="error">{(vm.error as Error).message}</Alert>;
  if (!vm.data) return <Alert severity="warning">VM not found.</Alert>;

  const v = vm.data;
  const boot =
    v.spec.boot.type === "direct_kernel"
      ? `direct kernel: ${v.spec.boot.kernel}`
      : `firmware: ${v.spec.boot.firmware}`;

  return (
    <Stack spacing={2}>
      <Stack direction="row" alignItems="center" justifyContent="space-between">
        <Stack direction="row" spacing={2} alignItems="center">
          <Typography variant="h5">{v.name}</Typography>
          <StatusChip value={v.phase} />
        </Stack>
        <Stack direction="row" spacing={1}>
          {can("vm:console") && (
            <Button
              component={RouterLink}
              to={`/vms/${id}/console`}
              startIcon={<TerminalIcon />}
              variant="outlined"
            >
              Console
            </Button>
          )}
          {can("vm:power") && (
            <>
              <Button
                startIcon={<PlayArrowIcon />}
                onClick={() => id && action.mutate({ id, action: "start" })}
                disabled={v.phase === "Running"}
              >
                Start
              </Button>
              <Button
                startIcon={<StopIcon />}
                onClick={() => id && action.mutate({ id, action: "stop" })}
                disabled={v.phase === "Stopped"}
              >
                Stop
              </Button>
            </>
          )}
          {can("vm:update") && (
            <Button startIcon={<EditIcon />} onClick={() => setEditOpen(true)}>
              Edit
            </Button>
          )}
          {can("vm:migrate") && (
            <Button
              startIcon={<SwapHorizIcon />}
              onClick={() => setMigrateOpen(true)}
              disabled={v.phase !== "Running"}
            >
              Migrate
            </Button>
          )}
          {can("vm:delete") && (
            <Button
              color="error"
              startIcon={<DeleteIcon />}
              onClick={() => id && action.mutate({ id, action: "delete" }, { onSuccess: () => navigate("/vms") })}
            >
              Delete
            </Button>
          )}
        </Stack>
      </Stack>

      {action.isError && <Alert severity="error">{(action.error as Error).message}</Alert>}
      {migrate.isError && <Alert severity="error">{(migrate.error as Error).message}</Alert>}

      {editOpen && <EditVmDialog vm={v} onClose={() => setEditOpen(false)} />}

      <Dialog open={migrateOpen} onClose={() => setMigrateOpen(false)} maxWidth="sm" fullWidth>
        <DialogTitle>Migrate “{v.name}”</DialogTitle>
        <DialogContent>
          <TextField
            select
            label="Target host"
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            fullWidth
            sx={{ mt: 1 }}
          >
            {(hosts.data ?? [])
              .filter((h) => h.state === "Ready" && h.schedulable && h.id !== v.host_id)
              .map((h) => (
                <MenuItem key={h.id} value={h.id}>
                  {h.name}
                </MenuItem>
              ))}
          </TextField>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setMigrateOpen(false)}>Cancel</Button>
          <Button
            variant="contained"
            disabled={!target || migrate.isPending}
            onClick={() =>
              id &&
              migrate.mutate(
                { id, targetHostId: target },
                { onSuccess: () => setMigrateOpen(false) },
              )
            }
          >
            Migrate
          </Button>
        </DialogActions>
      </Dialog>

      <Grid container spacing={2}>
        <Grid size={{ xs: 12, md: 6 }}>
          <Card>
            <CardContent>
              <Typography variant="h6" gutterBottom>
                Status
              </Typography>
              <Table size="small">
                <TableBody>
                  <Row label="Phase" value={<StatusChip value={v.phase} />} />
                  <Row label="Host" value={v.host_id ? shortId(v.host_id) : "unscheduled"} />
                  <Row label="IP" value={v.ip_address ?? "—"} />
                  <Row label="Message" value={v.message ?? "—"} />
                  <Row label="Generation" value={`${v.observed_generation} / ${v.generation}`} />
                  <Row label="Created" value={formatDate(v.created_at)} />
                </TableBody>
              </Table>
            </CardContent>
          </Card>
        </Grid>
        <Grid size={{ xs: 12, md: 6 }}>
          <Card>
            <CardContent>
              <Typography variant="h6" gutterBottom>
                Spec
              </Typography>
              <Table size="small">
                <TableBody>
                  <Row label="Desired power" value={v.spec.desired_power_state} />
                  <Row label="vCPUs" value={`${v.spec.cpu.boot_vcpus} (max ${v.spec.cpu.max_vcpus})`} />
                  <Row label="Memory" value={formatMib(v.spec.memory.size_mib)} />
                  <Row label="Boot" value={boot} />
                  <Row
                    label="Disks"
                    value={v.spec.disks.map((d, i) => (
                      <div key={i}>
                        {d.path}
                        {d.readonly ? " (ro)" : ""}
                      </div>
                    ))}
                  />
                  <Row label="NICs" value={<NicList vmId={v.id} nics={v.spec.network_interfaces} />} />
                </TableBody>
              </Table>
            </CardContent>
          </Card>
        </Grid>
        <Grid size={{ xs: 12, md: 6 }}>
          <MetricsCard vmId={v.id} />
        </Grid>
      </Grid>
    </Stack>
  );
}
