// Templates. The list; creating a VM from one is its own screen
// (/templates/:id/launch) because the override semantics need the room.

import { useState } from "react";
import { Link } from "react-router-dom";
import Dialog from "@mui/material/Dialog";
import {
  useCreateTemplate,
  useDeleteTemplate,
  useImages,
  useNetworks,
  useTemplates,
  useUpdateTemplate,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { ACTION } from "../auth/perm";
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
  PageHeader,
  QueryError,
  RowMenu,
  Select,
  SkeletonRows,
  Table,
  THead,
  TRow,
} from "../ui/kit";
import { formatBytes, formatMib } from "../format";
import type { CreateTemplateRequest, MachineType, Template } from "../api/types";

const GIB = 1024 * 1024 * 1024;
const COLS = "1.4fr 1.2fr 90px 100px 100px 1fr 110px";

function EditDialog({ edit, onClose }: { edit: Template | null; onClose: () => void }) {
  const images = useImages();
  const networks = useNetworks();
  const create = useCreateTemplate();
  const update = useUpdateTemplate();
  const [name, setName] = useState(edit?.name ?? "");
  const [imageId, setImageId] = useState(edit?.image_id ?? "");
  const [vcpus, setVcpus] = useState(String(edit?.boot_vcpus ?? 2));
  const [memMib, setMemMib] = useState(String(edit?.memory_mib ?? 2048));
  const [sizeGib, setSizeGib] = useState(
    edit?.disk_size_bytes ? String(Math.round(edit.disk_size_bytes / GIB)) : "10",
  );
  const [format, setFormat] = useState<"qcow2" | "raw">(edit?.disk_format ?? "qcow2");
  const [networkId, setNetworkId] = useState(edit?.network_id ?? "");
  const [machineType, setMachineType] = useState<MachineType>(edit?.machine_type ?? "standard");
  const [password, setPassword] = useState(edit?.cloud_init?.password ?? "");
  const [sshKey, setSshKey] = useState(edit?.cloud_init?.ssh_authorized_keys?.[0] ?? "");
  const isMicro = machineType === "microvm";

  const submit = () => {
    const body: CreateTemplateRequest = {
      name,
      image_id: imageId,
      boot_vcpus: Number(vcpus),
      max_vcpus: Number(vcpus),
      memory_mib: Number(memMib),
      disk_size_bytes: sizeGib ? Math.round(Number(sizeGib) * GIB) : null,
      disk_format: format,
      network_id: networkId || null,
      machine_type: machineType,
      // microVMs can't carry a cloud-init seed.
      cloud_init:
        !isMicro && (password || sshKey)
          ? { password: password || null, ssh_authorized_keys: sshKey ? [sshKey] : [] }
          : null,
    };
    if (edit) update.mutate({ id: edit.id, body }, { onSuccess: onClose });
    else create.mutate(body, { onSuccess: onClose });
  };

  const busy = create.isPending || update.isPending;
  const err = create.error || update.error;

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>{edit ? "Edit template" : "Create template"}</DialogHead>
      <DialogBody>
        <Field label="Name">
          <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label="Image">
          <Select value={imageId} onChange={(e) => setImageId(e.target.value)}>
            <option value="">— pick an image —</option>
            {(images.data ?? []).map((img) => (
              <option key={img.id} value={img.id}>
                {img.name}
              </option>
            ))}
          </Select>
        </Field>
        <Grid cols="1fr 1fr">
          <Field label="vCPU">
            <Input value={vcpus} onChange={(e) => setVcpus(e.target.value)} />
          </Field>
          <Field label="Memory (MiB)">
            <Input value={memMib} onChange={(e) => setMemMib(e.target.value)} />
          </Field>
        </Grid>
        <Grid cols="1fr 1fr">
          <Field label="Disk size (GiB)">
            <Input value={sizeGib} onChange={(e) => setSizeGib(e.target.value)} />
          </Field>
          <Field label="Disk format">
            <Select value={format} onChange={(e) => setFormat(e.target.value as "qcow2" | "raw")}>
              <option value="qcow2">qcow2 (thin overlay)</option>
              <option value="raw">raw (full copy)</option>
            </Select>
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
        <Field
          label="Machine type"
          help={
            isMicro
              ? "Minimal profile: requires a direct-kernel image and carries no cloud-init seed."
              : "Full device model."
          }
        >
          <Select
            value={machineType}
            onChange={(e) => setMachineType(e.target.value as MachineType)}
          >
            <option value="standard">standard</option>
            <option value="microvm">microvm</option>
          </Select>
        </Field>
        {!isMicro && (
          <Grid cols="1fr 1fr">
            <Field label="Default password">
              <Input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </Field>
            <Field label="Default SSH key">
              <Input value={sshKey} onChange={(e) => setSshKey(e.target.value)} />
            </Field>
          </Grid>
        )}
        {err && <ErrorPanel summary="Could not save the template" detail={err} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn kind="primary" onClick={submit} disabled={!name || !imageId || busy}>
          {edit ? "Save" : "Create"}
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

export function Templates() {
  const templates = useTemplates();
  const images = useImages();
  const networks = useNetworks();
  const del = useDeleteTemplate();
  const { can } = usePermissions();
  const [dialog, setDialog] = useState<{ edit: Template | null } | null>(null);

  const list = templates.data ?? [];
  const imageName = (id: string) => images.data?.find((i) => i.id === id)?.name ?? id.slice(0, 8);
  const networkName = (id: string | null) =>
    id ? (networks.data?.find((n) => n.id === id)?.name ?? id.slice(0, 8)) : null;

  return (
    <>
      <PageHeader
        title="Templates"
        subtitle={`${list.length} template${
          list.length === 1 ? "" : "s"
        } · a template pins an image, a size and a network so a VM is one form`}
        actions={
          can(ACTION.templateCreate) && (
            <Btn kind="primary" onClick={() => setDialog({ edit: null })}>
              Create template
            </Btn>
          )
        }
      />

      <QueryError error={templates.error} what="templates" />
      {del.isError && <ErrorPanel summary="Delete failed" detail={del.error} />}

      <Table>
        <THead cols={COLS}>
          <div>Template</div>
          <div>Image</div>
          <div>vCPU</div>
          <div>Memory</div>
          <div>Disk</div>
          <div>Network</div>
          <div>Machine</div>
        </THead>

        {templates.isLoading && <SkeletonRows cols={COLS} />}

        {!templates.isLoading && list.length === 0 && (
          <div style={{ padding: 18 }}>
            <EmptyState
              headline="No templates yet"
              hint="Create one from a ready image to make VM creation a single form."
            />
          </div>
        )}

        {list.map((t) => (
          <TRow key={t.id} cols={COLS}>
            <div className="vq-cell" style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <Link className="vq-name" to={`/templates/${t.id}/launch`}>
                {t.name}
              </Link>
              <RowMenu
                inline
                items={[
                  ...(can(ACTION.templateUpdate)
                    ? [{ label: "Edit", onClick: () => setDialog({ edit: t }) }]
                    : []),
                  ...(can(ACTION.templateDelete)
                    ? [{ label: "Delete", danger: true, onClick: () => del.mutate(t.id) }]
                    : []),
                ]}
              />
            </div>
            <div className="vq-cell vq-mono-sm">{imageName(t.image_id)}</div>
            <div className="vq-mono-sm">
              {t.boot_vcpus} / {t.max_vcpus}
            </div>
            <div className="vq-mono-sm">{formatMib(t.memory_mib)}</div>
            <div className="vq-mono-sm">
              {t.disk_size_bytes ? formatBytes(t.disk_size_bytes) : "image default"}
            </div>
            <div className="vq-cell vq-mono-sm">{networkName(t.network_id) ?? <Dash />}</div>
            <div className={`vq-mono-sm ${t.machine_type === "microvm" ? "t-cyan" : "t-3"}`}>
              {t.machine_type}
            </div>
          </TRow>
        ))}
      </Table>

      {dialog && <EditDialog edit={dialog.edit} onClose={() => setDialog(null)} />}
    </>
  );
}
