// Access control (handoff §13). Roles and group mappings sit side by side
// because that is the question an operator actually asks: who gets what, and
// how did they get it. The whole route is gated on iam:read; mutations need
// iam:manage and are enforced server-side regardless.

import { useMemo, useState } from "react";
import Dialog from "@mui/material/Dialog";
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
import { useAuth } from "../auth/AuthProvider";
import { usePermissions } from "../auth/permissions";
import { ACTION } from "../auth/perm";
import {
  Btn,
  Card,
  Check,
  Dash,
  DialogBody,
  DialogFoot,
  DialogHead,
  EmptyState,
  ErrorPanel,
  Field,
  Grid,
  Input,
  PageHeader,
  QueryError,
  RowMenu,
  Select,
  SkeletonRows,
  Table,
  THead,
  TRow,
} from "../ui/kit";
import type { RoleView, UserView } from "../api/types";

const ROLE_COLS = "1.2fr 80px 2fr";
const MAP_COLS = "1.4fr 1fr 90px";
const USER_COLS = "1.2fr 1.4fr 2fr 40px";

function groupByResource(catalog: string[]): Record<string, string[]> {
  const out: Record<string, string[]> = {};
  for (const p of catalog) {
    const [res] = p.split(":");
    (out[res] ??= []).push(p);
  }
  return out;
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
      if (next.has(p)) next.delete(p);
      else next.add(p);
      return next;
    });

  const busy = create.isPending || update.isPending;
  const err = create.error || update.error;

  return (
    <Dialog open onClose={onClose} maxWidth="md" fullWidth>
      <DialogHead>{edit ? `Edit role: ${edit.name}` : "Create custom role"}</DialogHead>
      <DialogBody>
        {!edit && (
          <Field label="Name">
            <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
          </Field>
        )}
        <Field label="Description">
          <Input value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        <div className="vq-label">Permissions</div>
        <Grid cols="repeat(auto-fill, minmax(200px, 1fr))">
          {Object.entries(grouped).map(([res, list]) => (
            <div
              key={res}
              style={{
                border: "1px solid var(--vq-line)",
                borderRadius: "var(--vq-radius-tile)",
                padding: 10,
              }}
            >
              <div className="vq-label">{res}</div>
              <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                {list.map((p) => (
                  <Check
                    key={p}
                    on={perms.has(p)}
                    label={p.split(":")[1]}
                    onChange={() => toggle(p)}
                  />
                ))}
              </div>
            </div>
          ))}
        </Grid>
        {err && <ErrorPanel summary="Could not save the role" detail={err} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={busy || (!edit && !name)}
          onClick={() => {
            const permissions = [...perms];
            if (edit)
              update.mutate(
                { id: edit.id, body: { description: description || null, permissions } },
                { onSuccess: onClose },
              );
            else
              create.mutate(
                { name, description: description || null, permissions },
                { onSuccess: onClose },
              );
          }}
        >
          {edit ? "Save" : "Create"}
        </Btn>
      </DialogFoot>
    </Dialog>
  );
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
  const [selected, setSelected] = useState<Set<string>>(() => new Set(user.roles.map((r) => r.id)));

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Roles for {user.username}</DialogHead>
      <DialogBody>
        {roles.map((r) => (
          <Check
            key={r.id}
            on={selected.has(r.id)}
            label={
              <span>
                {r.name}
                {r.builtin && <span className="t-3"> · built-in</span>}
                {r.description && (
                  <span className="vq-help" style={{ display: "block", marginTop: 0 }}>
                    {r.description}
                  </span>
                )}
              </span>
            }
            onChange={(on) =>
              setSelected((s) => {
                const next = new Set(s);
                if (on) next.add(r.id);
                else next.delete(r.id);
                return next;
              })
            }
          />
        ))}
        {setRoles.isError && <ErrorPanel summary="Could not save roles" detail={setRoles.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={setRoles.isPending}
          onClick={() =>
            setRoles.mutate({ userId: user.id, roleIds: [...selected] }, { onSuccess: onClose })
          }
        >
          Save
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function MapGroupDialog({ onClose }: { onClose: () => void }) {
  const roles = useIamRoles();
  const add = useAddGroupMapping();
  const [group, setGroup] = useState("");
  const [roleId, setRoleId] = useState("");

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Map an OIDC group to a role</DialogHead>
      <DialogBody>
        <Field
          label="OIDC group"
          help="Members inherit the role's permissions on their next login."
        >
          <Input value={group} autoFocus onChange={(e) => setGroup(e.target.value)} />
        </Field>
        <Field label="Role">
          <Select value={roleId} onChange={(e) => setRoleId(e.target.value)}>
            <option value="">— pick a role —</option>
            {(roles.data ?? []).map((r) => (
              <option key={r.id} value={r.id}>
                {r.name}
              </option>
            ))}
          </Select>
        </Field>
        {add.isError && <ErrorPanel summary="Mapping rejected" detail={add.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!group || !roleId || add.isPending}
          onClick={() => add.mutate({ group, roleId }, { onSuccess: onClose })}
        >
          Map
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

export function Iam() {
  const roles = useIamRoles();
  const users = useIamUsers();
  const mappings = useGroupMappings();
  const del = useDeleteRole();
  const remove = useRemoveGroupMapping();
  const { can } = usePermissions();
  const { enabled } = useAuth();
  const canManage = can(ACTION.iamManage);
  const [roleDialog, setRoleDialog] = useState<{ edit: RoleView | null } | null>(null);
  const [assign, setAssign] = useState<UserView | null>(null);
  const [mapping, setMapping] = useState(false);

  const roleList = roles.data ?? [];
  const builtins = roleList.filter((r) => r.builtin).length;

  // How many users currently hold each role — the closest honest answer to
  // "who does this mapping affect".
  const holders = useMemo(() => {
    const m = new Map<string, number>();
    (users.data ?? []).forEach((u) =>
      u.roles.forEach((r) => m.set(r.name, (m.get(r.name) ?? 0) + 1)),
    );
    return m;
  }, [users.data]);

  return (
    <>
      <PageHeader
        title="Access control"
        subtitle={
          enabled
            ? `Roles mapped from OIDC groups · ${builtins} built-in role${builtins === 1 ? "" : "s"}`
            : "Authentication is disabled on this control plane — every caller is a superuser."
        }
        actions={
          canManage && (
            <>
              <Btn onClick={() => setMapping(true)}>Map group</Btn>
              <Btn kind="primary" onClick={() => setRoleDialog({ edit: null })}>
                Create role
              </Btn>
            </>
          )
        }
      />

      <QueryError error={roles.error} what="roles" />
      {(del.isError || remove.isError) && (
        <ErrorPanel summary="Operation failed" detail={del.error || remove.error} />
      )}

      <Grid cols="1.1fr 1fr" className="vq-split">
        <Card title="Roles">
          <Table>
            <THead cols={ROLE_COLS}>
              <div>Role</div>
              <div>Type</div>
              <div>Permissions</div>
            </THead>
            {roles.isLoading && <SkeletonRows cols={ROLE_COLS} rows={4} />}
            {roleList.map((r) => (
              <TRow key={r.id} cols={ROLE_COLS}>
                <div className="vq-cell" style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <span className="vq-name">{r.name}</span>
                  <RowMenu
                    inline
                    items={
                      canManage && !r.builtin
                        ? [
                            { label: "Edit", onClick: () => setRoleDialog({ edit: r }) },
                            { label: "Delete", danger: true, onClick: () => del.mutate(r.id) },
                          ]
                        : []
                    }
                  />
                </div>
                <div className={`vq-mono-sm ${r.builtin ? "t-3" : "t-blue"}`}>
                  {r.builtin ? "builtin" : "custom"}
                </div>
                <div className="vq-pills">
                  {r.permissions.length === 0 && <Dash />}
                  {r.permissions.slice(0, 6).map((p) => (
                    <span key={p} className="vq-pill">
                      {p}
                    </span>
                  ))}
                  {r.permissions.length > 6 && (
                    <span className="vq-pill">+{r.permissions.length - 6}</span>
                  )}
                </div>
              </TRow>
            ))}
            {!roles.isLoading && roleList.length === 0 && (
              <div style={{ padding: 18 }}>
                <EmptyState headline="No roles" hint="Create one to grant a subset of permissions." />
              </div>
            )}
          </Table>
        </Card>

        <Card title="Group → role mappings" note="users counted by effective role">
          <Table>
            <THead cols={MAP_COLS}>
              <div>OIDC group</div>
              <div>Role</div>
              <div>Users</div>
            </THead>
            {mappings.isLoading && <SkeletonRows cols={MAP_COLS} rows={4} />}
            {(mappings.data ?? []).map((m) => (
              <TRow key={`${m.group}:${m.role}`} cols={MAP_COLS}>
                <div className="vq-cell vq-mono-sm">{m.group}</div>
                <div className="vq-cell" style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <span className="vq-name vq-mono-sm">{m.role}</span>
                  <RowMenu
                    inline
                    items={
                      canManage
                        ? [
                            {
                              label: "Remove mapping",
                              danger: true,
                              onClick: () => {
                                const r = roleList.find((x) => x.name === m.role);
                                if (r) remove.mutate({ group: m.group, roleId: r.id });
                              },
                            },
                          ]
                        : []
                    }
                  />
                </div>
                <div className="vq-mono-sm">{holders.get(m.role) ?? 0}</div>
              </TRow>
            ))}
            {!mappings.isLoading && (mappings.data ?? []).length === 0 && (
              <div style={{ padding: 18 }}>
                <EmptyState
                  headline="No group mappings"
                  hint="Map an OIDC group so members inherit a role on their next login."
                />
              </div>
            )}
          </Table>
        </Card>
      </Grid>

      <Card title="Users" note="roles granted directly, on top of any inherited from a group">
        <Table>
          <THead cols={USER_COLS}>
            <div>User</div>
            <div>Email</div>
            <div>Roles</div>
            <div />
          </THead>
          {users.isLoading && <SkeletonRows cols={USER_COLS} rows={3} />}
          {(users.data ?? []).map((u) => (
            <TRow key={u.id} cols={USER_COLS}>
              <div className="vq-cell vq-name">{u.username}</div>
              <div className="vq-cell vq-mono-sm">{u.email ?? <Dash />}</div>
              <div className="vq-pills">
                {u.roles.length === 0 && <Dash />}
                {u.roles.map((r) => (
                  <span key={r.id} className="vq-pill">
                    {r.name}
                  </span>
                ))}
              </div>
              <RowMenu
                inline
                items={
                  canManage && roles.data
                    ? [{ label: "Assign roles…", onClick: () => setAssign(u) }]
                    : []
                }
              />
            </TRow>
          ))}
          {!users.isLoading && (users.data ?? []).length === 0 && (
            <div style={{ padding: 18 }}>
              <EmptyState
                headline="No users yet"
                hint="A user appears the first time they sign in through the identity provider."
              />
            </div>
          )}
        </Table>
      </Card>

      {roleDialog && <RoleDialog edit={roleDialog.edit} onClose={() => setRoleDialog(null)} />}
      {assign && roles.data && (
        <AssignRolesDialog user={assign} roles={roles.data} onClose={() => setAssign(null)} />
      )}
      {mapping && <MapGroupDialog onClose={() => setMapping(false)} />}
    </>
  );
}
