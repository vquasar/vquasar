import { Link as RouterLink } from "react-router-dom";
import Box from "@mui/material/Box";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Grid from "@mui/material/Grid2";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import List from "@mui/material/List";
import ListItem from "@mui/material/ListItem";
import ListItemText from "@mui/material/ListItemText";
import { useEvents, useHosts, useNetworks, useTasks, useVms } from "../api/hooks";
import { StatusChip } from "../components/StatusChip";
import { formatDate } from "../format";

function StatCard({ title, value, to }: { title: string; value: string; to: string }) {
  return (
    <Card component={RouterLink} to={to} sx={{ textDecoration: "none", height: "100%" }}>
      <CardContent>
        <Typography variant="overline" color="text.secondary">
          {title}
        </Typography>
        <Typography variant="h4">{value}</Typography>
      </CardContent>
    </Card>
  );
}

export function Dashboard() {
  const hosts = useHosts();
  const vms = useVms();
  const tasks = useTasks();
  const events = useEvents();
  const networks = useNetworks();

  const readyHosts = (hosts.data ?? []).filter((h) => h.state === "Ready").length;
  const runningVms = (vms.data ?? []).filter((v) => v.phase === "Running").length;
  const openTasks = (tasks.data ?? []).filter(
    (t) => t.state === "Pending" || t.state === "Running",
  ).length;

  return (
    <Stack spacing={3}>
      <Typography variant="h5">Dashboard</Typography>
      <Grid container spacing={2}>
        <Grid size={{ xs: 6, md: 3 }}>
          <StatCard title="Hosts ready" value={`${readyHosts} / ${hosts.data?.length ?? 0}`} to="/hosts" />
        </Grid>
        <Grid size={{ xs: 6, md: 3 }}>
          <StatCard title="VMs running" value={`${runningVms} / ${vms.data?.length ?? 0}`} to="/vms" />
        </Grid>
        <Grid size={{ xs: 6, md: 3 }}>
          <StatCard title="Open tasks" value={String(openTasks)} to="/tasks" />
        </Grid>
        <Grid size={{ xs: 6, md: 3 }}>
          <StatCard title="Networks" value={String(networks.data?.length ?? 0)} to="/networks" />
        </Grid>
      </Grid>

      <Card>
        <CardContent>
          <Typography variant="h6" gutterBottom>
            Recent events
          </Typography>
          <List dense>
            {(events.data ?? []).slice(0, 12).map((e) => (
              <ListItem key={e.id} disableGutters>
                <Box sx={{ mr: 1 }}>
                  <StatusChip value={e.severity === "warning" ? "Maintenance" : "Running"} />
                </Box>
                <ListItemText
                  primary={`${e.event_type} — ${e.message}`}
                  secondary={formatDate(e.ts)}
                />
              </ListItem>
            ))}
            {(events.data ?? []).length === 0 && (
              <Typography variant="body2" color="text.secondary">
                No events yet.
              </Typography>
            )}
          </List>
        </CardContent>
      </Card>
    </Stack>
  );
}
