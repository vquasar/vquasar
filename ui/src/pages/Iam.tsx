// Identity & Access management (design M12b): assign roles to users, define
// custom roles from the permission catalog, and map OIDC groups to roles.
// Everything here requires iam:read; mutations require iam:manage (also
// enforced server-side).

import { useMemo, useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Checkbox from "@mui/material/Checkbox";
import Chip from "@mui/material/Chip";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import FormControlLabel from "@mui/material/FormControlLabel";
import FormGroup from "@mui/material/FormGroup";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import Tab from "@mui/material/Tab";
import Tabs from "@mui/material/Tabs";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import EditIcon from "@mui/icons-material/Edit";
import { DataGrid, GridActionsCellItem, type GridColDef } from "@mui/x-data-grid";
import {
  useAddGroupMapping,
  useCreateRole,
  useDeleteRole,
  useGroupMappings,
  useIamRoles,
  useIamUsers,
  usePermissionCatalog,
  useRemoveGroupMapping,
  useSetUserRoles,
  useUpdateRole,
} from "../api/iam";
import { usePermissions } from "../auth/permissions";
import type { RoleView, UserView } from "../api/types";

// Group catalog permissions by resource for a tidy picker.
function groupByResource(catalog: string[]): Record<string, string[]> {
  const out: Record<string, string[]> = {};
  for (const p of catalog) {
    const [res] = p.split(":");
    (out[res] ??= []).push(p);
  }
  return out;
}

function AssignRolesDialog({
  user,
  roles,
  onClose,
}: {
  user: UserView;
  roles: RoleView[];
  onClose: () => void;
}) {
  const setRoles = useSetUserRoles();
  const [selected, setSelected] = useState<Set<string>>(
    () => new Set(user.roles.map((r) => r.id)),
  );
  const toggle = (id: string) =>
    setSelected((s) => {
      const next = new Set(s);
      next.has(id) ? next.delete(id) : next.add(id);
      return next;
    });

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Roles for {user.username}</DialogTitle>
      <DialogContent>
        <FormGroup sx={{ mt: 1 }}>
          {roles.map((r) => (
            <FormControlLabel
              key={r.id}
              control={
                <Checkbox checked={selected.has(r.id)} onChange={() => toggle(r.id)} />
              }
              label={
                <span>
                  {r.name}{" "}
                  {r.builtin && <Chip label="built-in" size="small" sx={{ ml: 0.5 }} />}
                  {r.description && (
                    <Typography variant="caption" color="text.secondary" display="block">
                      {r.description}
                    </Typography>
                  )}
                </span>
              }
            />
          ))}
        </FormGroup>
        {setRoles.error && (
          <Alert severity="error">{(setRoles.error as Error).message}</Alert>
        )}
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button
          variant="contained"
          disabled={setRoles.isPending}
          onClick={() =>
            setRoles.mutate(
              { userId: user.id, roleIds: [...selected] },
              { onSuccess: onClose },
            )
          }
        >
          Save
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function UsersTab({ canManage }: { canManage: boolean }) {
  const users = useIamUsers();
  const roles = useIamRoles();
  const [assign, setAssign] = useState<UserView | null>(null);

  const cols: GridColDef<UserView>[] = [
    { field: "username", headerName: "User", flex: 1, minWidth: 160 },
    { field: "email", headerName: "Email", flex: 1, minWidth: 180 },
    {
      field: "roles",
      headerName: "Roles",
      flex: 2,
      minWidth: 220,
      sortable: false,
      renderCell: (p) => (
        <Stack direction="row" spacing={0.5} sx={{ flexWrap: "wrap", py: 0.5 }}>
          {p.row.roles.map((r) => (
            <Chip key={r.id} label={r.name} size="small" />
          ))}
        </Stack>
      ),
    },
    {
      field: "actions",
      type: "actions",
      headerName: "",
      width: 60,
      getActions: (p) =>
        canManage
          ? [
              <GridActionsCellItem
                key="edit"
                icon={<EditIcon />}
                label="Assign roles"
                onClick={() => setAssign(p.row)}
              />,
            ]
          : [],
    },
  ];

  return (
    <>
      {users.error && <Alert severity="error">{(users.error as Error).message}</Alert>}
      <DataGrid
        autoHeight
        rows={users.data ?? []}
        columns={cols}
        loading={users.isLoading}
        getRowId={(r) => r.id}
        disableRowSelectionOnClick
      />
      {assign && roles.data && (
        <AssignRolesDialog user={assign} roles={roles.data} onClose={() => setAssign(null)} />
      )}
    </>
  );
}

function RoleDialog({ edit, onClose }: { edit: RoleView | null; onClose: () => void }) {
  const catalog = usePermissionCatalog();
  const create = useCreateRole();
  const update = useUpdateRole();
  const [name, setName] = useState(edit?.name ?? "");
  const [description, setDescription] = useState(edit?.description ?? "");
  const [perms, setPerms] = useState<Set<string>>(() => new Set(edit?.permissions ?? []));
  const grouped = useMemo(() => groupByResource(catalog.data ?? []), [catalog.data]);

  const toggle = (p: string) =>
    setPerms((s) => {
      const next = new Set(s);
      next.has(p) ? next.delete(p) : next.add(p);
      return next;
    });

  const submit = () => {
    const permissions = [...perms];
    if (edit) {
      update.mutate(
        { id: edit.id, body: { description: description || null, permissions } },
        { onSuccess: onClose },
      );
    } else {
      create.mutate({ name, description: description || null, permissions }, { onSuccess: onClose });
    }
  };
  const busy = create.isPending || update.isPending;
  const err = (create.error || update.error) as Error | null;

  return (
    <Dialog open onClose={onClose} maxWidth="md" fullWidth>
      <DialogTitle>{edit ? `Edit role: ${edit.name}` : "Create custom role"}</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          {!edit && (
            <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          )}
          <TextField
            label="Description"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
          />
          <Typography variant="subtitle2">Permissions</Typography>
          <Box sx={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(220px, 1fr))", gap: 1 }}>
            {Object.entries(grouped).map(([res, list]) => (
              <Box key={res} sx={{ border: 1, borderColor: "divider", borderRadius: 1, p: 1 }}>
                <Typography variant="caption" color="text.secondary" sx={{ textTransform: "uppercase" }}>
                  {res}
                </Typography>
                <FormGroup>
                  {list.map((p) => (
                    <FormControlLabel
                      key={p}
                      control={
                        <Checkbox size="small" checked={perms.has(p)} onChange={() => toggle(p)} />
                      }
                      label={<Typography variant="body2">{p.split(":")[1]}</Typography>}
                    />
                  ))}
                </FormGroup>
              </Box>
            ))}
          </Box>
          {err && <Alert severity="error">{err.message}</Alert>}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="contained" onClick={submit} disabled={busy || (!edit && !name)}>
          {edit ? "Save" : "Create"}
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function RolesTab({ canManage }: { canManage: boolean }) {
  const roles = useIamRoles();
  const del = useDeleteRole();
  const [dialog, setDialog] = useState<{ edit: RoleView | null } | null>(null);

  const cols: GridColDef<RoleView>[] = [
    {
      field: "name",
      headerName: "Role",
      flex: 1,
      minWidth: 150,
      renderCell: (p) => (
        <span>
          {p.row.name}{" "}
          {p.row.builtin && <Chip label="built-in" size="small" sx={{ ml: 0.5 }} />}
        </span>
      ),
    },
    { field: "description", headerName: "Description", flex: 1.5, minWidth: 200 },
    {
      field: "permissions",
      headerName: "Permissions",
      flex: 1,
      minWidth: 120,
      valueGetter: (_v, row) => row.permissions.length,
      renderCell: (p) => `${p.row.permissions.length} granted`,
    },
    {
      field: "actions",
      type: "actions",
      headerName: "",
      width: 90,
      getActions: (p) => {
        if (!canManage || p.row.builtin) return [];
        return [
          <GridActionsCellItem
            key="edit"
            icon={<EditIcon />}
            label="Edit"
            onClick={() => setDialog({ edit: p.row })}
          />,
          <GridActionsCellItem
            key="del"
            icon={<DeleteIcon />}
            label="Delete"
            onClick={() => {
              if (confirm(`Delete role "${p.row.name}"?`)) del.mutate(p.row.id);
            }}
          />,
        ];
      },
    },
  ];

  return (
    <>
      <Box sx={{ display: "flex", mb: 2 }}>
        <Box sx={{ flexGrow: 1 }} />
        {canManage && (
          <Button startIcon={<AddIcon />} variant="contained" onClick={() => setDialog({ edit: null })}>
            New role
          </Button>
        )}
      </Box>
      {(roles.error || del.error) && (
        <Alert severity="error">{((roles.error || del.error) as Error).message}</Alert>
      )}
      <DataGrid
        autoHeight
        rows={roles.data ?? []}
        columns={cols}
        loading={roles.isLoading}
        getRowId={(r) => r.id}
        disableRowSelectionOnClick
      />
      {dialog && <RoleDialog edit={dialog.edit} onClose={() => setDialog(null)} />}
    </>
  );
}

function GroupsTab({ canManage }: { canManage: boolean }) {
  const mappings = useGroupMappings();
  const roles = useIamRoles();
  const add = useAddGroupMapping();
  const remove = useRemoveGroupMapping();
  const [group, setGroup] = useState("");
  const [roleName, setRoleName] = useState("");

  const roleId = useMemo(
    () => roles.data?.find((r) => r.name === roleName)?.id,
    [roles.data, roleName],
  );

  const cols: GridColDef[] = [
    { field: "group", headerName: "OIDC group", flex: 1, minWidth: 200 },
    { field: "role", headerName: "Role", flex: 1, minWidth: 160 },
    {
      field: "actions",
      type: "actions",
      headerName: "",
      width: 60,
      getActions: (p) =>
        canManage
          ? [
              <GridActionsCellItem
                key="del"
                icon={<DeleteIcon />}
                label="Remove"
                onClick={() => {
                  const r = roles.data?.find((x) => x.name === p.row.role);
                  if (r) remove.mutate({ group: p.row.group, roleId: r.id });
                }}
              />,
            ]
          : [],
    },
  ];

  return (
    <>
      <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
        Members of an OIDC group inherit the mapped role's permissions on their next login.
      </Typography>
      {canManage && (
        <Stack direction="row" spacing={1} sx={{ mb: 2 }} alignItems="center">
          <TextField
            size="small"
            label="Group"
            value={group}
            onChange={(e) => setGroup(e.target.value)}
          />
          <TextField
            size="small"
            select
            label="Role"
            value={roleName}
            onChange={(e) => setRoleName(e.target.value)}
            sx={{ minWidth: 160 }}
          >
            {(roles.data ?? []).map((r) => (
              <MenuItem key={r.id} value={r.name}>
                {r.name}
              </MenuItem>
            ))}
          </TextField>
          <Button
            startIcon={<AddIcon />}
            variant="contained"
            disabled={!group || !roleId || add.isPending}
            onClick={() =>
              roleId &&
              add.mutate({ group, roleId }, {
                onSuccess: () => {
                  setGroup("");
                  setRoleName("");
                },
              })
            }
          >
            Map
          </Button>
        </Stack>
      )}
      {(mappings.error || add.error || remove.error) && (
        <Alert severity="error">
          {((mappings.error || add.error || remove.error) as Error).message}
        </Alert>
      )}
      <DataGrid
        autoHeight
        rows={(mappings.data ?? []).map((m, i) => ({ id: i, ...m }))}
        columns={cols}
        loading={mappings.isLoading}
        disableRowSelectionOnClick
      />
    </>
  );
}

export function Iam() {
  const [tab, setTab] = useState(0);
  const { can } = usePermissions();
  const canManage = can("iam:manage");

  return (
    <Box>
      <Typography variant="h5" gutterBottom>
        Access control
      </Typography>
      <Tabs value={tab} onChange={(_e, v) => setTab(v)} sx={{ mb: 2 }}>
        <Tab label="Users" />
        <Tab label="Roles" />
        <Tab label="Groups" />
      </Tabs>
      {tab === 0 && <UsersTab canManage={canManage} />}
      {tab === 1 && <RolesTab canManage={canManage} />}
      {tab === 2 && <GroupsTab canManage={canManage} />}
    </Box>
  );
}
