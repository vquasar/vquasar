import { useNavigate, useParams } from "react-router-dom";
import Alert from "@mui/material/Alert";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Grid from "@mui/material/Grid2";
import Stack from "@mui/material/Stack";
import Table from "@mui/material/Table";
import TableBody from "@mui/material/TableBody";
import TableCell from "@mui/material/TableCell";
import TableRow from "@mui/material/TableRow";
import Typography from "@mui/material/Typography";
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import StopIcon from "@mui/icons-material/Stop";
import DeleteIcon from "@mui/icons-material/Delete";
import { useVm, useVmAction } from "../api/hooks";
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
            color="error"
            startIcon={<DeleteIcon />}
            onClick={() => id && action.mutate({ id, action: "delete" }, { onSuccess: () => navigate("/vms") })}
          >
            Delete
          </Button>
        </Stack>
      </Stack>

      {action.isError && <Alert severity="error">{(action.error as Error).message}</Alert>}

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
