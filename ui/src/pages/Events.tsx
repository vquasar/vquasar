import Alert from "@mui/material/Alert";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import { DataGrid, type GridColDef } from "@mui/x-data-grid";
import { useEvents } from "../api/hooks";
import { StatusChip } from "../components/StatusChip";
import { formatDate, shortId } from "../format";
import type { Event } from "../api/types";

const columns: GridColDef<Event>[] = [
  { field: "ts", headerName: "Time", width: 190, valueGetter: (v) => formatDate(v as string) },
  {
    field: "severity",
    headerName: "Severity",
    width: 110,
    renderCell: (p) => <StatusChip value={(p.value as string) === "warning" ? "Maintenance" : "Running"} />,
  },
  { field: "event_type", headerName: "Event", width: 170 },
  { field: "resource_type", headerName: "Resource", width: 110 },
  { field: "resource_id", headerName: "ID", width: 110, valueGetter: (v) => shortId(v as string | null) },
  { field: "message", headerName: "Message", flex: 1, minWidth: 200 },
];

export function Events() {
  const events = useEvents();
  return (
    <Stack spacing={2}>
      <Typography variant="h5">Events</Typography>
      {events.isError && <Alert severity="error">{(events.error as Error).message}</Alert>}
      <div style={{ height: 600, width: "100%" }}>
        <DataGrid
          rows={events.data ?? []}
          columns={columns}
          loading={events.isLoading}
          density="compact"
          disableRowSelectionOnClick
          initialState={{ pagination: { paginationModel: { pageSize: 50 } } }}
          pageSizeOptions={[50, 100, 200]}
        />
      </div>
    </Stack>
  );
}
