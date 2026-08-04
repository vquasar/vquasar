import { useMemo } from "react";
import { useNavigate } from "react-router-dom";
import Button from "@mui/material/Button";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import Alert from "@mui/material/Alert";
import AddIcon from "@mui/icons-material/Add";
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import StopIcon from "@mui/icons-material/Stop";
import DeleteIcon from "@mui/icons-material/Delete";
import { DataGrid, GridActionsCellItem, type GridColDef } from "@mui/x-data-grid";
import { useHosts, useVmAction, useVms } from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { StatusChip } from "../components/StatusChip";
import { formatDate, formatMib, shortId } from "../format";
import type { Vm } from "../api/types";

export function Vms() {
  const vms = useVms();
  const hosts = useHosts();
  const action = useVmAction();
  const navigate = useNavigate();
  const { can } = usePermissions();

  const hostName = useMemo(() => {
    const m = new Map<string, string>();
    (hosts.data ?? []).forEach((h) => m.set(h.id, h.name));
    return m;
  }, [hosts.data]);

  const columns: GridColDef<Vm>[] = [
    {
      field: "name",
      headerName: "Name",
      flex: 1,
      minWidth: 140,
      renderCell: (p) => (
        <Button size="small" onClick={() => navigate(`/vms/${p.row.id}`)} sx={{ textTransform: "none" }}>
          {p.value as string}
        </Button>
      ),
    },
    {
      field: "phase",
      headerName: "State",
      width: 120,
      renderCell: (p) => <StatusChip value={p.value as string} />,
    },
    {
      field: "host_id",
      headerName: "Host",
      width: 130,
      valueGetter: (v) => (v ? (hostName.get(v as string) ?? shortId(v as string)) : "—"),
    },
    {
      field: "vcpu",
      headerName: "vCPU",
      width: 80,
      valueGetter: (_v, row) => row.spec.cpu.boot_vcpus,
    },
    {
      field: "memory",
      headerName: "Memory",
      width: 110,
      valueGetter: (_v, row) => formatMib(row.spec.memory.size_mib),
    },
    {
      field: "ip_address",
      headerName: "IP",
      width: 130,
      valueGetter: (v) => (v as string | null) ?? "—",
    },
    {
      field: "created_at",
      headerName: "Created",
      width: 180,
      valueGetter: (v) => formatDate(v as string),
    },
    {
      field: "actions",
      type: "actions",
      headerName: "Actions",
      width: 130,
      getActions: (params) => {
        const items = [];
        if (can("vm:power")) {
          items.push(
            <GridActionsCellItem
              key="start"
              icon={<PlayArrowIcon />}
              label="Start"
              onClick={() => action.mutate({ id: params.row.id, action: "start" })}
              disabled={params.row.phase === "Running"}
            />,
            <GridActionsCellItem
              key="stop"
              icon={<StopIcon />}
              label="Stop"
              onClick={() => action.mutate({ id: params.row.id, action: "stop" })}
              disabled={params.row.phase === "Stopped"}
            />,
          );
        }
        if (can("vm:delete")) {
          items.push(
            <GridActionsCellItem
              key="delete"
              icon={<DeleteIcon />}
              label="Delete"
              onClick={() => action.mutate({ id: params.row.id, action: "delete" })}
              showInMenu
            />,
          );
        }
        return items;
      },
    },
  ];

  return (
    <Stack spacing={2}>
      <Stack direction="row" alignItems="center" justifyContent="space-between">
        <Typography variant="h5">Virtual Machines</Typography>
        {can("vm:create") && (
          <Button variant="contained" startIcon={<AddIcon />} onClick={() => navigate("/vms/new")}>
            Create VM
          </Button>
        )}
      </Stack>
      {vms.isError && <Alert severity="error">{(vms.error as Error).message}</Alert>}
      {action.isError && <Alert severity="error">{(action.error as Error).message}</Alert>}
      <div style={{ height: 560, width: "100%" }}>
        <DataGrid
          rows={vms.data ?? []}
          columns={columns}
          loading={vms.isLoading}
          density="compact"
          disableRowSelectionOnClick
          initialState={{ pagination: { paginationModel: { pageSize: 25 } } }}
          pageSizeOptions={[10, 25, 50]}
        />
      </div>
    </Stack>
  );
}
