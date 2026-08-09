// Storage pools (ADR-023).
//
// The page is shaped around the one thing a pool row cannot tell you: whether
// it works. Name, kind and path are what an operator typed; state, host count
// and free space are what the fleet reports back, and the two are kept visually
// apart for that reason. A pool that reads as perfectly configured and is
// reported by nobody is the situation this resource exists to make visible, so
// `pending` is a first-class state here rather than an absence.

import { useState } from "react";
import Dialog from "@mui/material/Dialog";
import {
  useCreateStoragePool,
  useDeleteStoragePool,
  useStoragePool,
  useStoragePools,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { ACTION, READ } from "../auth/perm";
import {
  Btn,
  Dash,
  DialogBody,
  DialogFoot,
  DialogHead,
  EmptyState,
  ErrorPanel,
  Field,
  Input,
  Grid,
  Mono,
  PageHeader,
  QueryError,
  RowMenu,
  Select,
  SkeletonRows,
  StateChip,
  Table,
  THead,
  TRow,
} from "../ui/kit";
import { formatBytes, formatDate } from "../format";
import type { StoragePool } from "../api/types";

type PoolKind = "shared_dir" | "local_dir" | "nfs";

const COLS = "1.4fr 110px 1.6fr 110px 90px 1fr 60px";

/// `local` is called out because it changes what the host count means and what
/// the VMs on it can do — not decoration.
function SharingChip({ pool }: { pool: StoragePool }) {
  return (
    <StateChip
      value={pool.sharing}
      tone={pool.sharing === "local" ? "amber" : "blue"}
      dense
      title={pool.sharing_note}
    />
  );
}

function CreateDialog({ onClose }: { onClose: () => void }) {
  const create = useCreateStoragePool();
  const [name, setName] = useState("");
  const [kind, setKind] = useState<PoolKind>("shared_dir");
  const [path, setPath] = useState("");
  const [server, setServer] = useState("");
  const [exportPath, setExportPath] = useState("");
  const [options, setOptions] = useState("");
  const [description, setDescription] = useState("");
  const named = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(name);
  const valid =
    named &&
    path.startsWith("/") &&
    (kind === "shared_dir" || (!!server && !server.includes(":") && exportPath.startsWith("/")));

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Add storage pool</DialogHead>
      <DialogBody>
        <Field
          label="Name"
          help="Lowercase letters, digits and dashes. Volumes refer to it by this."
        >
          <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field
          label="Kind"
          help="A shared directory is one you have already mounted on the hosts yourself; an NFS pool records the export and the agents mount it. A local directory is each host's own disk — fast, and a VM with a disk there cannot be live-migrated."
        >
          <Select
            value={kind}
            onChange={(e) => setKind(e.target.value as PoolKind)}
          >
            <option value="shared_dir">shared directory (already mounted)</option>
            <option value="local_dir">local directory (each host&apos;s own disk)</option>
            <option value="nfs">NFS export (mounted by the agents)</option>
          </Select>
        </Field>
        {kind === "nfs" && (
          <>
            <Grid cols="1fr 1fr">
              <Field label="Server" help="Address only — the export is separate.">
                <Input
                  value={server}
                  placeholder="10.0.0.5"
                  onChange={(e) => setServer(e.target.value)}
                />
              </Field>
              <Field label="Export">
                <Input
                  value={exportPath}
                  placeholder="/exports/vms"
                  onChange={(e) => setExportPath(e.target.value)}
                />
              </Field>
            </Grid>
            <Field label="Mount options" help="Optional, comma-separated.">
              <Input
                value={options}
                placeholder="vers=4.2,hard"
                onChange={(e) => setOptions(e.target.value)}
              />
            </Field>
          </>
        )}
        <Field
          label={kind === "nfs" ? "Mount point" : "Path"}
          help={
            kind === "nfs"
              ? "Where the agents mount the export. They create it if it is missing — a pool is only usable once the export is actually mounted there."
              : "Where the bytes go, as the hosts see it. The directory must already exist on a host for that host to report the pool — nothing here creates it."
          }
        >
          <Input
            value={path}
            placeholder={kind === "nfs" ? "/var/lib/vquasar/nfs/fast" : "/srv/fast"}
            onChange={(e) => setPath(e.target.value)}
          />
        </Field>
        <Field label="Description" help="Optional.">
          <Input value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        {create.isError && <ErrorPanel summary="Create rejected" detail={create.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!valid || create.isPending}
          onClick={() =>
            create.mutate(
              kind === "nfs"
                ? {
                    name,
                    kind,
                    server,
                    export: exportPath,
                    mount_point: path,
                    ...(options ? { options } : {}),
                    description: description || null,
                  }
                : { name, kind, path, description: description || null },
              { onSuccess: onClose },
            )
          }
        >
          {create.isPending ? "Creating…" : "Create"}
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

/// Every host's word on one pool. This is the answer to "why was my placement
/// refused", so an unusable host leads with its reason rather than a red dot.
function Reports({ id }: { id: string }) {
  const detail = useStoragePool(id);
  if (detail.isLoading) return <SkeletonRows cols="1fr 1fr" rows={2} />;
  if (detail.isError) return <QueryError error={detail.error} what="pool detail" />;
  const hosts = detail.data?.hosts ?? [];
  if (hosts.length === 0) {
    return (
      <EmptyState
        headline="No host has reported this pool"
        hint="A pool is usable only while some host says it is. Check that the directory exists and is writable on the hosts that should have it mounted — an agent that cannot use a pool says why here."
      />
    );
  }
  return (
    <Table style={{ margin: "8px 0 16px" }}>
      <THead cols="1fr 110px 2fr 1fr">
        <span>Host</span>
        <span>Usable</span>
        <span>Reported</span>
        <span>Free / total</span>
      </THead>
      {hosts.map((h) => (
        <TRow key={h.host_id} cols="1fr 110px 2fr 1fr">
          <span>{h.host_name}</span>
          <StateChip value={h.usable ? "ready" : "Failed"} dense />
          <span title={formatDate(h.reported_at)}>
            {h.usable ? "usable" : (h.message ?? "not usable")}
          </span>
          <span>
            {h.available_bytes !== null && h.capacity_bytes !== null ? (
              `${formatBytes(h.available_bytes)} / ${formatBytes(h.capacity_bytes)}`
            ) : (
              <Dash />
            )}
          </span>
        </TRow>
      ))}
    </Table>
  );
}

function Row({ pool, canManage }: { pool: StoragePool; canManage: boolean }) {
  const [open, setOpen] = useState(false);
  const del = useDeleteStoragePool();
  return (
    <>
      <TRow cols={COLS} onClick={() => setOpen((v) => !v)}>
        <span>
          {pool.name}
          {pool.description && (
            <span className="vq-sub" style={{ display: "block" }}>
              {pool.description}
            </span>
          )}
        </span>
        <span>
          {pool.kind}
          <span style={{ display: "block", marginTop: 2 }}>
            <SharingChip pool={pool} />
          </span>
        </span>
        <Mono>{pool.params.path ?? pool.params.mount_point ?? <Dash />}</Mono>
        <StateChip value={pool.state} dense />
        <span>{pool.reachable_hosts}</span>
        <span>
          {pool.available_bytes !== null && pool.capacity_bytes !== null ? (
            `${formatBytes(pool.available_bytes)} / ${formatBytes(pool.capacity_bytes)}`
          ) : (
            <Dash />
          )}
        </span>
        {canManage ? (
          <RowMenu
            items={[
              {
                label: "Delete",
                danger: true,
                onClick: () => del.mutate(pool.id),
              },
            ]}
          />
        ) : (
          <span />
        )}
      </TRow>
      {del.isError && <ErrorPanel summary="Delete rejected" detail={del.error} />}
      {open && <Reports id={pool.id} />}
    </>
  );
}

export function StoragePools() {
  const pools = useStoragePools();
  const { can } = usePermissions();
  const [creating, setCreating] = useState(false);
  const canManage = can(ACTION.poolCreate);

  if (!can(READ.storagePools)) {
    return (
      <EmptyState
        headline="You do not have access to storage pools"
        hint="Ask an administrator for storagepool:read."
      />
    );
  }

  return (
    <>
      <PageHeader
        title="Storage pools"
        subtitle="Where volumes put their bytes. Whether a pool works is reported by the hosts, not configured here."
        actions={
          canManage ? (
            <Btn kind="primary" onClick={() => setCreating(true)}>
              Add pool
            </Btn>
          ) : undefined
        }
      />
      {pools.isError && <QueryError error={pools.error} what="storage pools" />}
      <Table>
        <THead cols={COLS}>
          <span>Name</span>
          <span>Kind</span>
          <span>Path</span>
          <span>State</span>
          <span>Hosts</span>
          <span>Free / total</span>
          <span />
        </THead>
        {pools.isLoading && <SkeletonRows cols={COLS} rows={3} />}
        {pools.data?.map((p) => (
          <Row key={p.id} pool={p} canManage={canManage} />
        ))}
      </Table>
      {pools.data?.length === 0 && (
        <EmptyState
          headline="No storage pools"
          hint="A cluster gets a `default` pool from its configured shared directory the first time the control plane starts."
        />
      )}
      {creating && <CreateDialog onClose={() => setCreating(false)} />}
    </>
  );
}
