// Images (handoff §6). A card grid rather than a table: an image is a small
// object with a status that matters, and an import in flight deserves its own
// progress rather than a cell.

import { useState } from "react";
import { Link } from "react-router-dom";
import Dialog from "@mui/material/Dialog";
import {
  useCreateImage,
  useDeleteImage,
  useImages,
  useImportImage,
  useNetworks,
  useTemplates,
  useUpdateImage,
  useUploadImage,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import {
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
  RowMenu,
  Select,
  SkeletonRows,
  StateChip,
  Table,
  THead,
  TRow,
  Toggle,
} from "../ui/kit";
import { formatBytes, formatMib } from "../format";
import type { BootSpec, CreateImageRequest, Image } from "../api/types";

const GIB = 1024 * 1024 * 1024;
const TPL_COLS = "1.4fr 1.2fr 90px 100px 100px 1fr 110px";

function bootLabel(img: Image): string {
  const parts: string[] = [img.format];
  parts.push(img.boot.type === "firmware" ? "firmware boot" : "direct kernel");
  parts.push(img.cloud_init ? "cloud-init" : "no seed");
  return parts.join(" · ");
}

function EditDialog({ edit, onClose }: { edit: Image | null; onClose: () => void }) {
  const dk = edit?.boot.type === "direct_kernel" ? edit.boot : null;
  const fw = edit?.boot.type === "firmware" ? edit.boot : null;
  const [name, setName] = useState(edit?.name ?? "");
  const [os, setOs] = useState(edit?.os ?? "");
  const [sourcePath, setSourcePath] = useState(edit?.source_path ?? "");
  const [format, setFormat] = useState<"raw" | "qcow2">(edit?.format ?? "raw");
  const [bootType, setBootType] = useState<"direct_kernel" | "firmware">(
    edit?.boot.type ?? "direct_kernel",
  );
  const [kernel, setKernel] = useState(dk?.kernel ?? "");
  const [initramfs, setInitramfs] = useState(dk?.initramfs ?? "");
  const [cmdline, setCmdline] = useState(dk?.cmdline ?? "root=/dev/vda1 rw console=ttyS0");
  const [firmware, setFirmware] = useState(fw?.firmware ?? "");
  const [sizeGib, setSizeGib] = useState(
    edit?.default_size_bytes ? String(Math.round(edit.default_size_bytes / GIB)) : "",
  );
  const [cloudInit, setCloudInit] = useState(edit?.cloud_init ?? true);
  const create = useCreateImage();
  const update = useUpdateImage();

  const submit = () => {
    const boot: BootSpec =
      bootType === "direct_kernel"
        ? { type: "direct_kernel", kernel, initramfs: initramfs || null, cmdline: cmdline || null }
        : { type: "firmware", firmware };
    const body: CreateImageRequest = {
      name,
      source_path: sourcePath,
      format,
      boot,
      default_size_bytes: sizeGib ? Math.round(Number(sizeGib) * GIB) : null,
      cloud_init: cloudInit,
      os: os || null,
    };
    if (edit) update.mutate({ id: edit.id, body }, { onSuccess: onClose });
    else create.mutate(body, { onSuccess: onClose });
  };

  const busy = create.isPending || update.isPending;
  const err = create.error || update.error;
  const ready = name && sourcePath && (bootType === "firmware" ? firmware : kernel);

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>{edit ? "Edit image" : "Register image"}</DialogHead>
      <DialogBody>
        <Grid cols="1fr 1fr">
          <Field label="Name">
            <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label="OS label">
            <Input value={os} onChange={(e) => setOs(e.target.value)} placeholder="ubuntu-24.04" />
          </Field>
        </Grid>
        <Field
          label="Base disk path"
          help="On shared storage, e.g. /var/lib/vquasar/shared/images/ubuntu-24.04.raw"
        >
          <Input value={sourcePath} onChange={(e) => setSourcePath(e.target.value)} />
        </Field>
        <Grid cols="1fr 1fr">
          <Field label="Base format">
            <Select value={format} onChange={(e) => setFormat(e.target.value as "raw" | "qcow2")}>
              <option value="raw">raw</option>
              <option value="qcow2">qcow2</option>
            </Select>
          </Field>
          <Field label="Boot">
            <Select
              value={bootType}
              onChange={(e) => setBootType(e.target.value as "direct_kernel" | "firmware")}
            >
              <option value="direct_kernel">direct kernel</option>
              <option value="firmware">firmware (UEFI)</option>
            </Select>
          </Field>
        </Grid>
        {bootType === "direct_kernel" ? (
          <>
            <Field label="Kernel path">
              <Input value={kernel} onChange={(e) => setKernel(e.target.value)} />
            </Field>
            <Field label="Initramfs path">
              <Input value={initramfs} onChange={(e) => setInitramfs(e.target.value)} />
            </Field>
            <Field label="Kernel cmdline">
              <Input value={cmdline} onChange={(e) => setCmdline(e.target.value)} />
            </Field>
          </>
        ) : (
          <Field label="Firmware path">
            <Input value={firmware} onChange={(e) => setFirmware(e.target.value)} />
          </Field>
        )}
        <Field label="Default disk size (GiB)" help="Grow provisioned volumes to this size.">
          <Input value={sizeGib} onChange={(e) => setSizeGib(e.target.value)} />
        </Field>
        <Toggle on={cloudInit} onChange={setCloudInit} label="Expects a cloud-init seed" />
        {err && <ErrorPanel summary="Could not save the image" detail={err} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn kind="primary" onClick={submit} disabled={!ready || busy}>
          {edit ? "Save" : "Register"}
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function ImportDialog({ onClose }: { onClose: () => void }) {
  const imp = useImportImage();
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [format, setFormat] = useState<"raw" | "qcow2">("qcow2");
  const [os, setOs] = useState("");
  const [firmware, setFirmware] = useState("/var/lib/vquasar/firmware/CLOUDHV.fd");
  const [sizeGib, setSizeGib] = useState("");
  const [cloudInit, setCloudInit] = useState(true);

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Import image from URL</DialogHead>
      <DialogBody>
        <Field label="Name">
          <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label="URL">
          <Input
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="https://cloud-images.ubuntu.com/…/disk.img"
          />
        </Field>
        <Grid cols="1fr 1fr">
          <Field label="Format">
            <Select value={format} onChange={(e) => setFormat(e.target.value as "raw" | "qcow2")}>
              <option value="qcow2">qcow2</option>
              <option value="raw">raw</option>
            </Select>
          </Field>
          <Field label="OS label">
            <Input value={os} onChange={(e) => setOs(e.target.value)} placeholder="ubuntu-24.04" />
          </Field>
        </Grid>
        <Field label="Firmware (UEFI) path">
          <Input value={firmware} onChange={(e) => setFirmware(e.target.value)} />
        </Field>
        <Field label="Default size (GiB)">
          <Input value={sizeGib} onChange={(e) => setSizeGib(e.target.value)} />
        </Field>
        <Toggle on={cloudInit} onChange={setCloudInit} label="Uses cloud-init" />
        <div className="vq-help">
          The download runs in the background; the image becomes usable when it turns ready.
        </div>
        {imp.isError && <ErrorPanel summary="Import rejected" detail={imp.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!name || !url || imp.isPending}
          onClick={() =>
            imp.mutate(
              {
                name,
                url,
                format,
                os: os || null,
                cloud_init: cloudInit,
                boot: { type: "firmware", firmware },
                default_size_bytes: sizeGib ? Math.round(Number(sizeGib) * GIB) : null,
              },
              { onSuccess: onClose },
            )
          }
        >
          Import
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function UploadDialog({ onClose }: { onClose: () => void }) {
  const up = useUploadImage();
  const [name, setName] = useState("");
  const [format, setFormat] = useState<"raw" | "qcow2">("qcow2");
  const [os, setOs] = useState("");
  const [firmware, setFirmware] = useState("/var/lib/vquasar/firmware/CLOUDHV.fd");
  const [file, setFile] = useState<File | null>(null);

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Upload image</DialogHead>
      <DialogBody>
        <Field label="Disk file">
          <label className="vq-btn tall" style={{ justifyContent: "flex-start" }}>
            {file ? file.name : "Choose a disk file…"}
            <input
              type="file"
              hidden
              onChange={(e) => {
                const f = e.target.files?.[0] ?? null;
                setFile(f);
                if (f && !name) setName(f.name.replace(/\.(qcow2|img|raw)$/i, ""));
              }}
            />
          </label>
        </Field>
        <Field label="Name">
          <Input value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Grid cols="1fr 1fr">
          <Field label="Format">
            <Select value={format} onChange={(e) => setFormat(e.target.value as "raw" | "qcow2")}>
              <option value="qcow2">qcow2</option>
              <option value="raw">raw</option>
            </Select>
          </Field>
          <Field label="OS label">
            <Input value={os} onChange={(e) => setOs(e.target.value)} />
          </Field>
        </Grid>
        <Field label="Firmware (UEFI) path">
          <Input value={firmware} onChange={(e) => setFirmware(e.target.value)} />
        </Field>
        {up.isPending && (
          <div className="vq-warnpanel">Uploading — keep this dialog open until it finishes.</div>
        )}
        {up.isError && <ErrorPanel summary="Upload failed" detail={up.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!name || !file || up.isPending}
          onClick={() =>
            file &&
            up.mutate(
              { params: { name, format, os, firmware, cloud_init: "true" }, file },
              { onSuccess: onClose },
            )
          }
        >
          Upload
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function ImageCard({
  img,
  derived,
  menu,
}: {
  img: Image;
  derived: number;
  menu: { label: string; onClick: () => void; danger?: boolean }[];
}) {
  const importing = img.status === "importing";
  const failed = img.status === "failed";

  return (
    <div
      className="vq-card"
      style={{
        borderColor: importing
          ? "var(--vq-cyan-line)"
          : failed
            ? "var(--vq-red-line)"
            : undefined,
      }}
    >
      <div className="vq-card-body">
        <div style={{ display: "flex", alignItems: "flex-start", gap: 10 }}>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontSize: 14, fontWeight: 600 }}>{img.name}</div>
            <div className="vq-mono-sm" style={{ fontSize: 10.5, marginTop: 3 }}>
              {bootLabel(img)}
            </div>
          </div>
          <StateChip value={img.status} dense title={img.error ?? undefined} />
          <RowMenu items={menu} />
        </div>

        <div style={{ marginTop: 14 }}>
          {importing ? (
            <>
              <div style={{ display: "flex", justifyContent: "space-between", gap: 10 }}>
                <span className="vq-mono-sm">transferred</span>
                <span className="vq-mono-sm t-cyan">
                  {img.size_bytes != null ? formatBytes(img.size_bytes) : "…"}
                  {img.default_size_bytes ? ` / ${formatBytes(img.default_size_bytes)}` : ""}
                </span>
              </div>
              <div className="vq-bar thick" style={{ marginTop: 8 }}>
                <span
                  className="vq-bar-cyan vq-pulse-fast"
                  style={{
                    width: `${
                      img.size_bytes && img.default_size_bytes
                        ? Math.min(100, (img.size_bytes / img.default_size_bytes) * 100)
                        : 40
                    }%`,
                  }}
                />
              </div>
              <div
                style={{ display: "flex", justifyContent: "space-between", gap: 10, marginTop: 10 }}
              >
                <span className="vq-mono-sm">source</span>
                <span className="vq-mono-sm vq-cell">{img.source_path}</span>
              </div>
            </>
          ) : failed ? (
            <ErrorPanel summary="Import failed" detail={img.error} />
          ) : (
            <>
              <Row k="size" v={formatBytes(img.size_bytes)} />
              <Row k="default disk" v={formatBytes(img.default_size_bytes)} />
              <Row k="derived volumes" v={String(derived)} />
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function Row({ k, v }: { k: string; v: string }) {
  return (
    <div style={{ display: "flex", justifyContent: "space-between", gap: 10, padding: "5px 0" }}>
      <span className="vq-mono-sm">{k}</span>
      <span className="vq-mono-sm t-2">{v}</span>
    </div>
  );
}

export function Images() {
  const images = useImages();
  const templates = useTemplates();
  const networks = useNetworks();
  const del = useDeleteImage();
  const { can } = usePermissions();
  const [dialog, setDialog] = useState<{ edit: Image | null } | null>(null);
  const [importing, setImporting] = useState(false);
  const [uploading, setUploading] = useState(false);

  const list = images.data ?? [];
  const inFlight = list.filter((i) => i.status === "importing").length;
  const derivedCount = (imageId: string) =>
    (templates.data ?? []).filter((t) => t.image_id === imageId).length;
  const imageName = (id: string) => list.find((i) => i.id === id)?.name ?? id.slice(0, 8);
  const networkName = (id: string | null) =>
    id ? (networks.data?.find((n) => n.id === id)?.name ?? id.slice(0, 8)) : null;

  return (
    <>
      <PageHeader
        title="Images"
        subtitle={`${list.length} image${list.length === 1 ? "" : "s"}${
          inFlight ? ` · ${inFlight} importing` : ""
        } · managed images are reference-counted`}
        actions={
          can("image:create") && (
            <>
              <Btn onClick={() => setUploading(true)}>Upload</Btn>
              <Btn onClick={() => setDialog({ edit: null })}>Register</Btn>
              <Btn kind="primary" onClick={() => setImporting(true)}>
                Import image
              </Btn>
            </>
          )
        }
      />

      <QueryError error={images.error} what="images" />
      {del.isError && <ErrorPanel summary="Delete failed" detail={del.error} />}

      {images.isLoading ? (
        <Grid cols="repeat(3, 1fr)">
          {[0, 1, 2].map((i) => (
            <div key={i} className="vq-card">
              <div className="vq-card-body">
                <div className="vq-skel" style={{ width: "60%" }} />
                <div className="vq-skel" style={{ width: "90%", marginTop: 12 }} />
                <div className="vq-skel" style={{ width: "40%", marginTop: 8 }} />
              </div>
            </div>
          ))}
        </Grid>
      ) : list.length === 0 ? (
        <EmptyState
          headline="No images yet"
          hint="Import one from a URL, upload a disk, or register a path on shared storage."
        />
      ) : (
        <Grid cols="repeat(3, 1fr)">
          {list.map((img) => (
            <ImageCard
              key={img.id}
              img={img}
              derived={derivedCount(img.id)}
              menu={
                can("image:create")
                  ? [
                      { label: "Edit", onClick: () => setDialog({ edit: img }) },
                      { label: "Delete", danger: true, onClick: () => del.mutate(img.id) },
                    ]
                  : []
              }
            />
          ))}
        </Grid>
      )}

      <Card
        title="Templates"
        actions={
          <Link to="/templates" className="vq-card-note" style={{ color: "var(--vq-blue)" }}>
            Manage
          </Link>
        }
      >
        <Table>
          <THead cols={TPL_COLS}>
            <div>Template</div>
            <div>Image</div>
            <div>vCPU</div>
            <div>Memory</div>
            <div>Disk</div>
            <div>Network</div>
            <div>Machine</div>
          </THead>
          {templates.isLoading && <SkeletonRows cols={TPL_COLS} rows={3} />}
          {!templates.isLoading && (templates.data ?? []).length === 0 && (
            <div style={{ padding: 18 }}>
              <EmptyState
                headline="No templates"
                hint="A template pins an image, a size and a network so a VM is one click."
              />
            </div>
          )}
          {(templates.data ?? []).map((t) => (
            <TRow key={t.id} cols={TPL_COLS}>
              <div className="vq-cell">
                <Link className="vq-name" to={`/templates/${t.id}/launch`}>
                  {t.name}
                </Link>
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
              {/* microvm is the moving part worth noticing here. */}
              <div className={`vq-mono-sm ${t.machine_type === "microvm" ? "t-cyan" : "t-3"}`}>
                {t.machine_type}
              </div>
            </TRow>
          ))}
        </Table>
      </Card>

      {dialog && <EditDialog edit={dialog.edit} onClose={() => setDialog(null)} />}
      {importing && <ImportDialog onClose={() => setImporting(false)} />}
      {uploading && <UploadDialog onClose={() => setUploading(false)} />}
    </>
  );
}
