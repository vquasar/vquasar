import Alert from "@mui/material/Alert";
import LinearProgress from "@mui/material/LinearProgress";
import Box from "@mui/material/Box";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import { DataGrid, type GridColDef } from "@mui/x-data-grid";
import { useTasks } from "../api/hooks";
import { StatusChip } from "../components/StatusChip";
import { formatDate, shortId } from "../format";
import type { Task } from "../api/types";

const columns: GridColDef<Task>[] = [
  { field: "task_type", headerName: "Type", width: 140 },
  {
    field: "state",
    headerName: "State",
    width: 120,
    renderCell: (p) => <StatusChip value={p.value as string} />,
  },
  {
    field: "progress",
    headerName: "Progress",
    width: 160,
    renderCell: (p) => (
      <Box sx={{ display: "flex", alignItems: "center", gap: 1, width: "100%" }}>
        <LinearProgress variant="determinate" value={p.value as number} sx={{ flexGrow: 1 }} />
        <span>{p.value as number}%</span>
      </Box>
    ),
  },
  { field: "vm_id", headerName: "VM", width: 120, valueGetter: (v) => shortId(v as string | null) },
  { field: "message", headerName: "Message", flex: 1, minWidth: 180, valueGetter: (v) => v ?? "—" },
  { field: "created_at", headerName: "Created", width: 190, valueGetter: (v) => formatDate(v as string) },
];

export function Tasks() {
  const tasks = useTasks();
  return (
    <Stack spacing={2}>
      <Typography variant="h5">Tasks</Typography>
      {tasks.isError && <Alert severity="error">{(tasks.error as Error).message}</Alert>}
      <div style={{ height: 560, width: "100%" }}>
        <DataGrid
          rows={tasks.data ?? []}
          columns={columns}
          loading={tasks.isLoading}
          density="compact"
          disableRowSelectionOnClick
          initialState={{ pagination: { paginationModel: { pageSize: 25 } } }}
          pageSizeOptions={[25, 50, 100]}
        />
      </div>
    </Stack>
  );
}
