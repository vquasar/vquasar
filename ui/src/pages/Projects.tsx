// Projects — the unit of tenancy (design §47, ADR-018/019).
//
// Master/detail like security groups: a project is a small object whose
// interesting content is its quota, and a quota is five numbers that only mean
// something next to what is currently being used. Showing limits in a table and
// usage somewhere else would leave the operator doing the subtraction.
//
// This page is about *shaping* tenancy. Choosing which project you are working
// in is the top-bar switcher, not here.

import { useEffect, useState } from "react";
import Dialog from "@mui/material/Dialog";
import {
  useClearQuota,
  useCreateProject,
  useDeleteProject,
  useProjects,
  useQuota,
  useSetQuota,
  useUpdateProject,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { useProject } from "../auth/ProjectProvider";
import { ACTION } from "../auth/perm";
import { formatBytes } from "../format";
import type { Project, QuotaLimits } from "../api/types";
import {
  Bar,
  Btn,
  Card,
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
  SkeletonRows,
  Table,
} from "../ui/kit";

/// The five dimensions, in the order they appear everywhere else.
const DIMENSIONS = [
  { key: "vms", limit: "max_vms", label: "Virtual machines" },
  { key: "vcpus", limit: "max_vcpus", label: "vCPUs" },
  { key: "memory_mib", limit: "max_memory_mib", label: "Memory (MiB)" },
  { key: "volumes", limit: "max_volumes", label: "Volumes" },
  { key: "storage_bytes", limit: "max_storage_bytes", label: "Storage" },
] as const;

function CreateProjectDialog({ onClose }: { onClose: () => void }) {
  const create = useCreateProject();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>New project</DialogHead>
      <DialogBody>
        <Field
          label="Name"
          help="Lowercase letters, digits and dashes. Used in the API as well as here."
        >
          <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label="Description">
          <Input value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        {create.isError && <ErrorPanel summary="Create rejected" detail={create.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!name || create.isPending}
          onClick={() =>
            create.mutate({ name, description: description || null }, { onSuccess: onClose })
          }
        >
          Create
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function EditProjectDialog({ project, onClose }: { project: Project; onClose: () => void }) {
  const update = useUpdateProject();
  const [name, setName] = useState(project.name);
  const [description, setDescription] = useState(project.description ?? "");
  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Edit {project.name}</DialogHead>
      <DialogBody>
        <Field
          label="Name"
          help={
            project.is_default
              ? "This is the default project. Renaming it is allowed; what makes it the default is a flag, not the name."
              : undefined
          }
        >
          <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label="Description">
          <Input value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        {update.isError && <ErrorPanel summary="Update rejected" detail={update.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!name || update.isPending}
          onClick={() =>
            update.mutate(
              { id: project.id, body: { name, description: description || null } },
              { onSuccess: onClose },
            )
          }
        >
          Save
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

/// A blank field is unlimited, which is also how the API reads an omitted one.
/// The whole object is written on save, so there is no way to leave a stale
/// limit behind by forgetting to mention it.
function QuotaDialog({ project, onClose }: { project: Project; onClose: () => void }) {
  const quota = useQuota(project.id);
  const save = useSetQuota();
  const [fields, setFields] = useState<Record<string, string>>({});

  // Seed from what is already set, once it arrives.
  useEffect(() => {
    const l = quota.data?.limits;
    if (!l) return;
    setFields({
      max_vms: l.max_vms?.toString() ?? "",
      max_vcpus: l.max_vcpus?.toString() ?? "",
      max_memory_mib: l.max_memory_mib?.toString() ?? "",
      max_volumes: l.max_volumes?.toString() ?? "",
      max_storage_bytes: l.max_storage_bytes?.toString() ?? "",
    });
  }, [quota.data]);

  const submit = () => {
    const body: QuotaLimits = {};
    for (const d of DIMENSIONS) {
      const raw = fields[d.limit]?.trim();
      (body as Record<string, number | null>)[d.limit] =
        raw === "" || raw === undefined ? null : Number(raw);
    }
    save.mutate({ id: project.id, body }, { onSuccess: onClose });
  };

  const invalid = DIMENSIONS.some((d) => {
    const raw = fields[d.limit]?.trim();
    if (!raw) return false;
    return !/^\d+$/.test(raw);
  });

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Quota for {project.name}</DialogHead>
      <DialogBody>
        <p className="vq-hint" style={{ marginTop: 0 }}>
          Blank is unlimited. A limit counts what the project has committed to —
          a resource counts from the moment it exists, including while it is
          pending or being deleted. Lowering a limit below current usage is
          allowed: it blocks new work and destroys nothing.
        </p>
        <Grid cols="1fr 1fr">
          {DIMENSIONS.map((d) => (
            <Field
              key={d.limit}
              label={d.label}
              help={
                d.key === "storage_bytes"
                  ? "Bytes. Counts volumes and disks a VM asks to have provisioned."
                  : d.key === "vcpus"
                    ? "Counts each VM's maximum, not what it boots with."
                    : undefined
              }
            >
              <Input
                value={fields[d.limit] ?? ""}
                placeholder="unlimited"
                onChange={(e) => setFields({ ...fields, [d.limit]: e.target.value })}
              />
            </Field>
          ))}
        </Grid>
        {invalid && <ErrorPanel summary="Limits must be whole numbers, or blank for unlimited" />}
        {save.isError && <ErrorPanel summary="Quota rejected" detail={save.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn kind="primary" disabled={invalid || save.isPending} onClick={submit}>
          Save quota
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function fmt(dimension: string, value: number): string {
  return dimension === "storage_bytes" ? formatBytes(value) : value.toLocaleString();
}

function QuotaPanel({ project }: { project: Project }) {
  const quota = useQuota(project.id);
  const clear = useClearQuota();
  const { can } = usePermissions();
  const [editing, setEditing] = useState(false);

  const limits = quota.data?.limits;
  const usage = quota.data?.usage;
  const capped = DIMENSIONS.some((d) => limits?.[d.limit] != null);

  return (
    <>
      <Card
        title="Quota"
        desc={
          capped
            ? "Checked when work is admitted, against what the project has committed to."
            : "No limits — this project can use whatever the fleet has."
        }
        actions={
          can(ACTION.quotaSet) && (
            <div style={{ display: "flex", gap: 8 }}>
              <Btn onClick={() => setEditing(true)}>{capped ? "Edit quota" : "Set quota"}</Btn>
              {capped && (
                <Btn kind="destructive" onClick={() => clear.mutate(project.id)}>
                  Remove limits
                </Btn>
              )}
            </div>
          )
        }
      >
        {quota.isLoading ? (
          <Table>
            <SkeletonRows cols="1fr 1fr" rows={5} />
          </Table>
        ) : (
          <div style={{ padding: "4px 16px 16px" }}>
            {quota.data?.over_quota && (
              <ErrorPanel
                summary="Over quota"
                detail="A limit is below current usage — new work is refused until usage falls or the limit is raised. Nothing has been deleted."
              />
            )}
            {DIMENSIONS.map((d) => {
              const used = usage?.[d.key] ?? 0;
              const limit = limits?.[d.limit] ?? null;
              return (
                <div key={d.key} style={{ padding: "10px 0" }}>
                  <div
                    style={{
                      display: "flex",
                      justifyContent: "space-between",
                      fontSize: 12.5,
                      marginBottom: 6,
                    }}
                  >
                    <span style={{ color: "var(--vq-text-2)" }}>{d.label}</span>
                    <span className="vq-mono-sm">
                      {fmt(d.key, used)}
                      {limit == null ? (
                        <span style={{ color: "var(--vq-text-3)" }}> / unlimited</span>
                      ) : (
                        <> / {fmt(d.key, limit)}</>
                      )}
                    </span>
                  </div>
                  {limit != null && limit > 0 && (
                    <Bar
                      segments={[
                        {
                          pct: Math.min(100, (used / limit) * 100),
                          // Amber once the project is at or past its cap: the
                          // next request in that dimension is the one that
                          // gets refused.
                          tone: used >= limit ? "amber" : "blue",
                        },
                      ]}
                    />
                  )}
                </div>
              );
            })}
          </div>
        )}
        <QueryError error={quota.error} what="quota" />
      </Card>
      {editing && <QuotaDialog project={project} onClose={() => setEditing(false)} />}
    </>
  );
}

export function Projects() {
  const projects = useProjects();
  const del = useDeleteProject();
  const { can } = usePermissions();
  const { project: active, setProject } = useProject();
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const list = projects.data ?? [];
  const selected = list.find((p) => p.id === selectedId) ?? list[0];

  return (
    <>
      <PageHeader
        title="Projects"
        subtitle={`${list.length} project${list.length === 1 ? "" : "s"} · a project owns its VMs, volumes, templates and security groups`}
        actions={
          can(ACTION.projectCreate) && (
            <Btn kind="primary" onClick={() => setCreating(true)}>
              Create project
            </Btn>
          )
        }
      />

      <QueryError error={projects.error} what="projects" />
      {del.isError && <ErrorPanel summary="Delete refused" detail={del.error} />}

      {projects.isLoading ? (
        <Table>
          <SkeletonRows cols="1fr 1fr 1fr" />
        </Table>
      ) : list.length === 0 ? (
        <EmptyState
          headline="No projects visible"
          hint="You can act only in projects you hold a role in. An administrator can create one."
        />
      ) : (
        <Grid cols="280px 1fr" className="vq-split">
          <Card title="Projects">
            <div style={{ padding: 8 }}>
              {list.map((p) => (
                <button
                  key={p.id}
                  onClick={() => setSelectedId(p.id)}
                  style={{
                    display: "flex",
                    width: "100%",
                    alignItems: "center",
                    justifyContent: "space-between",
                    gap: 8,
                    padding: "8px 10px",
                    borderRadius: "var(--vq-radius-control)",
                    border: 0,
                    cursor: "pointer",
                    fontSize: 12.5,
                    background: p.id === selected?.id ? "var(--vq-blue-soft)" : "transparent",
                    color: p.id === selected?.id ? "var(--vq-blue)" : "var(--vq-text-2)",
                    fontWeight: p.id === selected?.id ? 500 : 400,
                  }}
                >
                  <span>{p.name}</span>
                  {p.is_default && <span className="vq-mono-sm">default</span>}
                </button>
              ))}
            </div>
          </Card>

          {selected && (
            <div style={{ display: "grid", gap: 16 }}>
              <Card
                title={selected.name}
                desc={selected.description ?? undefined}
                actions={
                  <div style={{ display: "flex", gap: 8 }}>
                    {active !== selected.id && (
                      <Btn onClick={() => setProject(selected.id)}>Switch to this project</Btn>
                    )}
                    {can(ACTION.projectUpdate) && <Btn onClick={() => setEditing(true)}>Edit</Btn>}
                    {can(ACTION.projectDelete) && !selected.is_default && (
                      <Btn kind="destructive" onClick={() => del.mutate(selected.id)}>
                        Delete project
                      </Btn>
                    )}
                  </div>
                }
              >
                <div style={{ padding: "0 16px 16px" }}>
                  <p className="vq-hint">
                    {selected.is_default
                      ? "The default project: every caller without a project context resolves here, and it cannot be deleted."
                      : "Deleting a project is refused while it still owns anything — the refusal says what is in the way."}
                  </p>
                  <div className="vq-mono-sm" style={{ color: "var(--vq-text-3)" }}>
                    {selected.id}
                  </div>
                </div>
              </Card>

              <QuotaPanel project={selected} />
            </div>
          )}
        </Grid>
      )}

      {creating && <CreateProjectDialog onClose={() => setCreating(false)} />}
      {editing && selected && (
        <EditProjectDialog project={selected} onClose={() => setEditing(false)} />
      )}
      {!list.length && <Dash />}
    </>
  );
}
