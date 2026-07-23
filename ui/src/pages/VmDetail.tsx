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
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import StopIcon from "@mui/icons-material/Stop";
import DeleteIcon from "@mui/icons-material/Delete";
import TerminalIcon from "@mui/icons-material/Terminal";
import SwapHorizIcon from "@mui/icons-material/SwapHoriz";
import { useHosts, useMigrateVm, useVm, useVmAction } from "../api/hooks";
import { StatusChip } from "../components/StatusChip";
import { formatDate, formatMib, shortId } from "../format";

function Row({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <TableRow>
      <TableCell sx={{ color: "text.secondary", width: 180, border: 0 }}>{label}</TableCell>
      <TableCell sx={{ border: 0 }}>{value}</TableCell>
    </TableRow>
  );
}

export function VmDetail() {
  const { id } = useParams();
  const navigate = useNavigate();
  const vm = useVm(id);
  const action = useVmAction();
  const hosts = useHosts();
  const migrate = useMigrateVm();
  const [migrateOpen, setMigrateOpen] = useState(false);
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
          <Button
            component={RouterLink}
            to={`/vms/${id}/console`}
            startIcon={<TerminalIcon />}
            variant="outlined"
          >
            Console
          </Button>
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
          <Button
            startIcon={<SwapHorizIcon />}
            onClick={() => setMigrateOpen(true)}
            disabled={v.phase !== "Running"}
          >
            Migrate
          </Button>
          <Button
            color="error"
            startIcon={<DeleteIcon />}
            onClick={() => id && action.mutate({ id, action: "delete" }, { onSuccess: () => navigate("/vms") })}
          >
            Delete
          </Button>
        </Stack>
      </Stack>

      {action.isError && <Alert severity="error">{(action.error as Error).message}</Alert>}
      {migrate.isError && <Alert severity="error">{(migrate.error as Error).message}</Alert>}

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
                  <Row label="NICs" value={v.spec.network_interfaces.length} />
                </TableBody>
              </Table>
            </CardContent>
          </Card>
        </Grid>
      </Grid>
    </Stack>
  );
}
