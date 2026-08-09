// Volumes (handoff §8). The snapshot count *is* a column now: it arrives on the
// volume row from a single aggregate, so the page costs the same one request it
// always did. It was left off while the only way to get it was one request per
// row — a page that got slower the more storage you had.

import { useState } from "react";
import { Link } from "react-router-dom";
import Dialog from "@mui/material/Dialog";
import {
  useAttachVolume,
  useCreateSnapshot,
  useCreateVmFromVolume,
  useCreateVolume,
  useDeleteSnapshot,
  useDeleteVolume,
  useDetachVolume,
  useImages,
  useNetworks,
  useStoragePools,
  useRevertSnapshot,
  useVms,
  useVolumes,
  useVolumeSnapshots,
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
  Grid,
  Input,
  Pagination,
  PageHeader,
  QueryError,
  RowMenu,
  Select,
  SkeletonRows,
  Table,
  THead,
  TRow,
} from "../ui/kit";
import { formatBytes, formatDate } from "../format";
import type { Vm, Volume } from "../api/types";

const GIB = 1024 * 1024 * 1024;
const COLS = "1.6fr 100px 90px 1.3fr 90px 1fr 1fr";
const PAGE_SIZE = 25;

function CreateDialog({ onClose }: { onClose: () => void }) {
  const create = useCreateVolume();
  const images = useImages();
  const pools = useStoragePools();
  const [pool, setPool] = useState("");
  const [name, setName] = useState("");
  const [source, setSource] = useState<"blank" | "image">("blank");
  const [imageId, setImageId] = useState("");
  const [gib, setGib] = useState("10");
  const [format, setFormat] = useState("qcow2");
  const readyImages = (images.data ?? []).filter((i) => i.status === "ready");
  const valid = !!name && (source === "blank" ? Number(gib) > 0 : !!imageId);

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Create volume</DialogHead>
      <DialogBody>
        <Field label="Name">
          <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label="Source">
          <Select
            value={source}
            onChange={(e) => setSource(e.target.value as "blank" | "image")}
          >
            <option value="blank">blank data volume</option>
            <option value="image">clone from image (bootable)</option>
          </Select>
        </Field>
        {source === "image" && (
          <Field label="Image">
            <Select value={imageId} onChange={(e) => setImageId(e.target.value)}>
              <option value="">— pick an image —</option>
              {readyImages.map((i) => (
                <option key={i.id} value={i.id}>
                  {i.name} ({i.format})
                </option>
              ))}
            </Select>
          </Field>
        )}
        {(pools.data?.length ?? 0) > 1 && (
          <Field
            label="Storage pool"
            help="Where the bytes go. A pool no host reports is still a legal place to put a volume — but no VM using it can be scheduled until some host does."
          >
            <Select value={pool} onChange={(e) => setPool(e.target.value)}>
              <option value="">default</option>
              {(pools.data ?? [])
                .filter((p) => p.name !== "default")
                .map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                    {p.state === "pending" ? " (no host reports it)" : ""}
                  </option>
                ))}
            </Select>
          </Field>
        )}
        <Grid cols="1fr 1fr">
          <Field
            label="Size (GiB)"
            help={source === "image" ? "Optional — grows the clone." : undefined}
          >
            <Input value={gib} onChange={(e) => setGib(e.target.value)} />
          </Field>
          {source === "blank" && (
            <Field label="Format">
              <Select value={format} onChange={(e) => setFormat(e.target.value)}>
                <option value="qcow2">qcow2 (thin)</option>
                <option value="raw">raw</option>
              </Select>
            </Field>
          )}
        </Grid>
        {create.isError && <ErrorPanel summary="Create rejected" detail={create.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!valid || create.isPending}
          onClick={() =>
            create.mutate(
              {
                name,
                size_bytes: gib ? Math.round(Number(gib) * GIB) : 0,
                format,
                ...(pool ? { pool } : {}),
                source_image_id: source === "image" ? imageId : null,
              },
              { onSuccess: onClose },
            )
          }
        >
          Create
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function BootDialog({ volume, onClose }: { volume: Volume; onClose: () => void }) {
  const boot = useCreateVmFromVolume();
  const networks = useNetworks();
  const [name, setName] = useState("");
  const [vcpus, setVcpus] = useState("2");
  const [memMib, setMemMib] = useState("2048");
  const [networkId, setNetworkId] = useState("");
  const [password, setPassword] = useState("");

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Boot a VM from {volume.name}</DialogHead>
      <DialogBody>
        <Field label="VM name">
          <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </Field>
        <Grid cols="1fr 1fr">
          <Field label="vCPU">
            <Input value={vcpus} onChange={(e) => setVcpus(e.target.value)} />
          </Field>
          <Field label="Memory (MiB)">
            <Input value={memMib} onChange={(e) => setMemMib(e.target.value)} />
          </Field>
        </Grid>
        <Field label="Network">
          <Select value={networkId} onChange={(e) => setNetworkId(e.target.value)}>
            <option value="">— none —</option>
            {(networks.data ?? []).map((n) => (
              <option key={n.id} value={n.id}>
                {n.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="cloud-init password">
          <Input type="password" value={password} onChange={(e) => setPassword(e.target.value)} />
        </Field>
        <div className="vq-help">
          The volume becomes the VM's root disk and stays attached; it survives VM deletion.
        </div>
        {boot.isError && <ErrorPanel summary="Boot rejected" detail={boot.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!name || boot.isPending}
          onClick={() =>
            boot.mutate(
              {
                name,
                volume_id: volume.id,
                boot_vcpus: Number(vcpus),
                max_vcpus: Number(vcpus),
                memory_mib: Number(memMib),
                network_id: networkId || null,
                cloud_init: password ? { password } : null,
              },
              { onSuccess: onClose },
            )
          }
        >
          Boot
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function AttachDialog({ volume, vms, onClose }: { volume: Volume; vms: Vm[]; onClose: () => void }) {
  const attach = useAttachVolume();
  const [vmId, setVmId] = useState("");
  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Attach {volume.name}</DialogHead>
      <DialogBody>
        <Field label="VM" help="Hot-adds to a running VM; otherwise attaches on the next start.">
          <Select value={vmId} onChange={(e) => setVmId(e.target.value)}>
            <option value="">— pick a VM —</option>
            {vms.map((v) => (
              <option key={v.id} value={v.id}>
                {v.name} ({v.phase})
              </option>
            ))}
          </Select>
        </Field>
        {attach.isError && <ErrorPanel summary="Attach failed" detail={attach.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!vmId || attach.isPending}
          onClick={() => attach.mutate({ id: volume.id, vmId }, { onSuccess: onClose })}
        >
          Attach
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function SnapshotsDialog({
  volume,
  canManage,
  onClose,
}: {
  volume: Volume;
  canManage: boolean;
  onClose: () => void;
}) {
  const snaps = useVolumeSnapshots(volume.id);
  const create = useCreateSnapshot();
  const del = useDeleteSnapshot();
  const revert = useRevertSnapshot();
  const [name, setName] = useState("");
  const err = create.error || del.error || revert.error;
  const qcow2 = volume.format === "qcow2";

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Snapshots — {volume.name}</DialogHead>
      <DialogBody>
        {!qcow2 && <div className="vq-warnpanel">Snapshots require a qcow2 volume.</div>}
        {err && <ErrorPanel summary="Snapshot operation failed" detail={err} />}
        {canManage && qcow2 && (
          <div style={{ display: "flex", gap: 8, alignItems: "flex-end" }}>
            <div style={{ flex: 1 }}>
              <Field label="Snapshot name">
                <Input value={name} onChange={(e) => setName(e.target.value)} />
              </Field>
            </div>
            <Btn
              tall
              disabled={!name || create.isPending}
              onClick={() =>
                create.mutate({ volumeId: volume.id, name }, { onSuccess: () => setName("") })
              }
            >
              Take
            </Btn>
          </div>
        )}
        <Table>
          <THead cols="1.4fr 1.2fr 150px">
            <div>Snapshot</div>
            <div>Created</div>
            <div />
          </THead>
          {(snaps.data ?? []).map((s) => (
            <TRow key={s.id} cols="1.4fr 1.2fr 150px">
              <div className="vq-cell">{s.name}</div>
              <div className="vq-mono-sm">{formatDate(s.created_at)}</div>
              <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
                {canManage && (
                  <>
                    <Btn
                      style={{ height: 24 }}
                      onClick={() => revert.mutate({ volumeId: volume.id, snapId: s.id })}
                    >
                      Revert
                    </Btn>
                    <Btn
                      kind="destructive"
                      style={{ height: 24 }}
                      onClick={() => del.mutate({ volumeId: volume.id, snapId: s.id })}
                    >
                      Delete
                    </Btn>
                  </>
                )}
              </div>
            </TRow>
          ))}
          {snaps.data?.length === 0 && (
            <div style={{ padding: 18 }}>
              <EmptyState headline="No snapshots" hint="Take one before a risky change." />
            </div>
          )}
        </Table>
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Close</Btn>
      </DialogFoot>
    </Dialog>
  );
}

export function Volumes() {
  const volumes = useVolumes();
  const vms = useVms();
  const images = useImages();
  const pools = useStoragePools();
  const del = useDeleteVolume();
  const detach = useDetachVolume();
  const { can } = usePermissions();
  const [creating, setCreating] = useState(false);
  const [attachVol, setAttachVol] = useState<Volume | null>(null);
  const [snapVol, setSnapVol] = useState<Volume | null>(null);
  const [bootVol, setBootVol] = useState<Volume | null>(null);
  const [page, setPage] = useState(1);

  /// A volume's pool, named — but only when it is not the default one. A
  /// volume that predates pools has none, and its bytes are in the default
  /// location, so it reads the same way.
  const poolName = (id: string | null) => {
    const p = (pools.data ?? []).find((x) => x.id === id);
    return p && p.name !== "default" ? p.name : null;
  };

  const list = volumes.data ?? [];
  // A fleet carries hundreds of volumes; render a page of them.
  const pages = Math.max(1, Math.ceil(list.length / PAGE_SIZE));
  const shown = list.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE);
  const provisioned = list.reduce((n, v) => n + v.size_bytes, 0);
  const vmName = (id: string | null) =>
    id ? (vms.data?.find((v) => v.id === id)?.name ?? id.slice(0, 8)) : null;
  const imageName = (id: string | null) =>
    id ? (images.data?.find((i) => i.id === id)?.name ?? id.slice(0, 8)) : null;

  return (
    <>
      <PageHeader
        title="Volumes"
        subtitle={`${list.length} volume${list.length === 1 ? "" : "s"} · ${formatBytes(
          provisioned,
        )} provisioned`}
        actions={
          can(ACTION.volumeCreate) && (
            <Btn kind="primary" onClick={() => setCreating(true)}>
              Create volume
            </Btn>
          )
        }
      />

      <QueryError error={volumes.error} what="volumes" />
      {(del.isError || detach.isError) && (
        <ErrorPanel summary="Volume operation failed" detail={del.error || detach.error} />
      )}

      <Table>
        <THead cols={COLS}>
          <div>Volume</div>
          <div>Size</div>
          <div>Format</div>
          <div>Attached to</div>
          <div>Serial</div>
          <div>Source image</div>
          <div>Snapshots</div>
        </THead>

        {volumes.isLoading && <SkeletonRows cols={COLS} />}

        {!volumes.isLoading && list.length === 0 && (
          <div style={{ padding: 18 }}>
            <EmptyState
              headline="No volumes yet"
              hint="Create a volume or clone an image to derive one."
            />
          </div>
        )}

        {shown.map((v) => {
          const attachedTo = vmName(v.attached_vm_id);
          const menu = [
            ...(can(READ.volumes)
              ? [{ label: "Snapshots…", onClick: () => setSnapVol(v) }]
              : []),
            ...(can(ACTION.vmCreate) && v.source_image_id && !v.attached_vm_id
              ? [{ label: "Boot a VM from this", onClick: () => setBootVol(v) }]
              : []),
            ...(can(ACTION.volumeUpdate)
              ? v.attached_vm_id
                ? [{ label: "Detach", onClick: () => detach.mutate(v.id) }]
                : [{ label: "Attach…", onClick: () => setAttachVol(v) }]
              : []),
            ...(can(ACTION.volumeDelete) && !v.attached_vm_id
              ? [{ label: "Delete", danger: true, onClick: () => del.mutate(v.id) }]
              : []),
          ];

          return (
            <TRow key={v.id} cols={COLS}>
              <div className="vq-cell" style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <span className="vq-name">{v.name}</span>
                {/* Only when it is somewhere other than the default pool. A
                    column repeating "default" on every row says nothing;
                    "this one is elsewhere" is the fact worth surfacing. */}
                {poolName(v.pool_id) && (
                  <span className="vq-sub">in {poolName(v.pool_id)}</span>
                )}
                <RowMenu inline items={menu} />
              </div>
              <div className="vq-mono-sm">{formatBytes(v.size_bytes)}</div>
              <div className="vq-mono-sm">{v.format}</div>
              <div className="vq-cell vq-mono-sm">
                {attachedTo ? (
                  <Link className="vq-name" to={`/vms/${v.attached_vm_id}`}>
                    {attachedTo}
                  </Link>
                ) : (
                  <span className="t-4">unattached</span>
                )}
              </div>
              <div className="vq-mono-sm">{v.attached_serial ?? <Dash />}</div>
              <div className="vq-cell vq-mono-sm">{imageName(v.source_image_id) ?? <Dash />}</div>
              <div className="vq-cell">
                {/* The count first, so the column answers "does this volume
                    have any" at a glance; the action stays where it was. */}
                <span className="vq-mono-sm">{v.snapshot_count || <Dash />}</span>
                <button
                  className="vq-btn link"
                  style={{ marginLeft: 8 }}
                  onClick={() => setSnapVol(v)}
                >
                  manage
                </button>
              </div>
            </TRow>
          );
        })}

        {list.length > 0 && (
          <Pagination
            page={page}
            pages={pages}
            shown={shown.length}
            total={list.length}
            onPage={setPage}
          />
        )}
      </Table>

      {creating && <CreateDialog onClose={() => setCreating(false)} />}
      {attachVol && (
        <AttachDialog volume={attachVol} vms={vms.data ?? []} onClose={() => setAttachVol(null)} />
      )}
      {snapVol && (
        <SnapshotsDialog
          volume={snapVol}
          canManage={can(ACTION.volumeUpdate)}
          onClose={() => setSnapVol(null)}
        />
      )}
      {bootVol && <BootDialog volume={bootVol} onClose={() => setBootVol(null)} />}
    </>
  );
}
