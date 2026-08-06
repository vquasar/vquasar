// VM detail (handoff §5). While a guest is migrating the banner owns the top of
// the screen: an operator watching a live migration should not have to hunt for
// the progress.

import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import Dialog from "@mui/material/Dialog";
import {
  useChangeNic,
  useEvents,
  useHosts,
  useMigrateVm,
  useNetworks,
  useSecurityGroups,
  useTasks,
  useUpdateVm,
  useVm,
  useVmAction,
  useVmMetrics,
  useVolumes,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { ACTION } from "../auth/perm";
import { useCrumb } from "../components/Breadcrumb";
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
  KV,
  PageHeader,
  ProgressCell,
  QueryError,
  Select,
  SkeletonRows,
  StateChip,
  Table,
  THead,
  TRow,
} from "../ui/kit";
import { duration, formatBytes, formatMib, formatTime } from "../format";
import type { Host, UpdateVmRequest, Vm } from "../api/types";

const GIB = 1024 * 1024 * 1024;
const NIC_COLS = "1fr 1fr 1fr 1fr";
const VOL_COLS = "1.4fr 1fr 1fr 1fr";
const TASK_COLS = "1fr 1.3fr 1fr 1fr 1fr";
const TABS = ["Overview", "Spec", "Networking", "Storage", "Tasks", "Events"] as const;
type Tab = (typeof TABS)[number];

/// Client-side mirror of the server's CPU migration gate (design M15): a guest
/// can migrate to `target` only if it has the same vendor and a superset of the
/// source host's guest-visible CPU features. Advisory — the control plane
/// enforces it — but it lets us annotate incompatible targets before the click.
type CpuVerdict =
  | { kind: "ok" }
  | { kind: "unknown" }
  | { kind: "vendor"; source: string; target: string }
  | { kind: "missing"; missing: string[] };

function cpuCompat(source: Host | undefined, target: Host): CpuVerdict {
  if (!source) return { kind: "unknown" };
  const sv = source.cpu_vendor;
  const tv = target.cpu_vendor;
  if (sv && tv && sv !== tv) return { kind: "vendor", source: sv, target: tv };
  if (!sv || !tv) return { kind: "unknown" };
  const sf = source.cpu_features ?? [];
  const tf = target.cpu_features ?? [];
  if (sf.length === 0 || tf.length === 0) return { kind: "unknown" };
  const have = new Set(tf);
  const missing = sf.filter((f) => !have.has(f));
  return missing.length ? { kind: "missing", missing } : { kind: "ok" };
}

function verdictLabel(v: CpuVerdict): string {
  switch (v.kind) {
    case "ok":
      return "CPU-compatible";
    case "unknown":
      return "CPU features unknown";
    case "vendor":
      return `vendor mismatch (${v.source} → ${v.target})`;
    case "missing":
      return `missing: ${v.missing.join(", ")}`;
  }
}

/// A rolling window of the last 12 samples, so the sparkline shows a trend
/// rather than a single reading. Never interpolated — each bar is one poll.
function useHistory(value: number | undefined, len = 12): number[] {
  const ref = useRef<number[]>([]);
  const [, force] = useState(0);
  useEffect(() => {
    if (value == null || Number.isNaN(value)) return;
    ref.current = [...ref.current, value].slice(-len);
    force((n) => n + 1);
  }, [value, len]);
  return ref.current;
}

function Spark({ values, cyan }: { values: number[]; cyan?: boolean }) {
  const max = Math.max(1, ...values);
  const pad = Math.max(0, 12 - values.length);
  return (
    <div className={`vq-spark${cyan ? " cyan" : ""}`}>
      {/* Slots we have no sample for stay empty — a flat bar would claim a
          reading that was never taken. */}
      {Array.from({ length: pad }, (_, i) => (
        <i key={`pad${i}`} className="empty" />
      ))}
      {values.map((v, i) => (
        <i
          key={i}
          style={{
            height: `${Math.max(8, (v / max) * 100)}%`,
            opacity: cyan && i < values.length - 6 ? 0.5 : 1,
          }}
        />
      ))}
    </div>
  );
}

function EditVmDialog({ vm, onClose }: { vm: Vm; onClose: () => void }) {
  const update = useUpdateVm();
  const networks = useNetworks();
  const [name, setName] = useState(vm.name);
  const [bootVcpus, setBootVcpus] = useState(String(vm.spec.cpu.boot_vcpus));
  const [maxVcpus, setMaxVcpus] = useState(String(vm.spec.cpu.max_vcpus));
  const [memMib, setMemMib] = useState(String(vm.spec.memory.size_mib));
  const [maxMemMib, setMaxMemMib] = useState(
    vm.spec.memory.max_size_mib ? String(vm.spec.memory.max_size_mib) : "",
  );
  const [growIdx, setGrowIdx] = useState("");
  const [growGib, setGrowGib] = useState("");
  const [addDiskGib, setAddDiskGib] = useState("");
  const [addNic, setAddNic] = useState("");

  const writableDisks = vm.spec.disks.map((d, i) => ({ d, i })).filter(({ d }) => !d.readonly);

  const submit = () => {
    const body: UpdateVmRequest = {};
    if (name !== vm.name) body.name = name;
    if (Number(bootVcpus) !== vm.spec.cpu.boot_vcpus) body.boot_vcpus = Number(bootVcpus);
    if (Number(maxVcpus) !== vm.spec.cpu.max_vcpus) body.max_vcpus = Number(maxVcpus);
    if (Number(memMib) !== vm.spec.memory.size_mib) body.memory_mib = Number(memMib);
    if (maxMemMib && Number(maxMemMib) !== (vm.spec.memory.max_size_mib ?? 0))
      body.memory_max_mib = Number(maxMemMib);
    if (growIdx !== "" && growGib)
      body.grow_disk = { index: Number(growIdx), size_bytes: Math.round(Number(growGib) * GIB) };
    if (addDiskGib)
      body.add_disk = { size_bytes: Math.round(Number(addDiskGib) * GIB), image_type: "qcow2" };
    if (addNic) body.add_nic = { network_id: addNic };
    update.mutate({ id: vm.id, body }, { onSuccess: onClose });
  };

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Edit {vm.name}</DialogHead>
      <DialogBody>
        <Field label="Name">
          <Input value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Grid cols="1fr 1fr">
          <Field label="vCPU" help={`hot-plug up to max ${maxVcpus}`}>
            <Input value={bootVcpus} onChange={(e) => setBootVcpus(e.target.value)} />
          </Field>
          <Field label="Max vCPU" help="raising needs a restart">
            <Input value={maxVcpus} onChange={(e) => setMaxVcpus(e.target.value)} />
          </Field>
        </Grid>
        <Grid cols="1fr 1fr">
          <Field
            label="Memory (MiB)"
            help={maxMemMib ? `hot-plug up to ${maxMemMib}` : "restart to change"}
          >
            <Input value={memMib} onChange={(e) => setMemMib(e.target.value)} />
          </Field>
          <Field label="Max memory (MiB)" help="enables live resize; needs a restart">
            <Input value={maxMemMib} onChange={(e) => setMaxMemMib(e.target.value)} />
          </Field>
        </Grid>
        <Grid cols="1fr 1fr">
          <Field label="Grow disk">
            <Select value={growIdx} onChange={(e) => setGrowIdx(e.target.value)}>
              <option value="">— none —</option>
              {writableDisks.map(({ d, i }) => (
                <option key={i} value={String(i)}>
                  {d.path.split("/").pop()}
                  {d.size_bytes ? ` (${formatBytes(d.size_bytes)})` : ""}
                </option>
              ))}
            </Select>
          </Field>
          <Field label="New size (GiB)" help="applied on the next Stop → Start">
            <Input value={growGib} onChange={(e) => setGrowGib(e.target.value)} />
          </Field>
        </Grid>
        <Field label="Add data disk (GiB)" help="blank qcow2, hot-added">
          <Input value={addDiskGib} onChange={(e) => setAddDiskGib(e.target.value)} />
        </Field>
        <Field label="Add NIC on network">
          <Select value={addNic} onChange={(e) => setAddNic(e.target.value)}>
            <option value="">— none —</option>
            {(networks.data ?? []).map((n) => (
              <option key={n.id} value={n.id}>
                {n.name}
              </option>
            ))}
          </Select>
        </Field>
        {update.isError && <ErrorPanel summary="Update rejected" detail={update.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn kind="primary" onClick={submit} disabled={update.isPending}>
          Apply
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function MigrateDialog({ vm, onClose }: { vm: Vm; onClose: () => void }) {
  const hosts = useHosts();
  const migrate = useMigrateVm();
  const [target, setTarget] = useState("");
  const [force, setForce] = useState(false);

  const sourceHost = (hosts.data ?? []).find((h) => h.id === vm.host_id);
  const candidates = (hosts.data ?? []).filter(
    (h) => h.state === "Ready" && h.schedulable && h.id !== vm.host_id,
  );
  const targetHost = candidates.find((h) => h.id === target);
  const verdict = targetHost ? cpuCompat(sourceHost, targetHost) : null;
  const incompatible = verdict?.kind === "vendor" || verdict?.kind === "missing";

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Migrate {vm.name}</DialogHead>
      <DialogBody>
        <Field label="Target host">
          <Select value={target} onChange={(e) => setTarget(e.target.value)}>
            <option value="">— pick a host —</option>
            {candidates.map((h) => {
              const vd = cpuCompat(sourceHost, h);
              const bad = vd.kind === "vendor" || vd.kind === "missing";
              return (
                <option key={h.id} value={h.id} disabled={bad && !force}>
                  {h.name} — {verdictLabel(vd)}
                </option>
              );
            })}
          </Select>
        </Field>
        {incompatible && verdict && (
          <div className="vq-warnpanel">
            Target CPU is not compatible with the source: {verdictLabel(verdict)}. Cloud Hypervisor
            cannot mask CPU features, so the guest may crash if it uses one the target lacks.
          </div>
        )}
        <Check on={force} onChange={setForce} label="Force migrate despite CPU incompatibility" />
        {migrate.isError && <ErrorPanel summary="Migration rejected" detail={migrate.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!target || migrate.isPending}
          onClick={() =>
            migrate.mutate({ id: vm.id, targetHostId: target, force }, { onSuccess: onClose })
          }
        >
          Migrate
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function ChangeNicDialog({
  vm,
  index,
  onClose,
}: {
  vm: Vm;
  index: number;
  onClose: () => void;
}) {
  const networks = useNetworks();
  const securityGroups = useSecurityGroups();
  const change = useChangeNic();
  const nic = vm.spec.network_interfaces[index];
  const [networkId, setNetworkId] = useState(nic?.network_id ?? "");
  const [sgIds, setSgIds] = useState<string[]>(nic?.security_groups ?? []);

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Change eth{index} network</DialogHead>
      <DialogBody>
        <Field label="Network">
          <Select value={networkId} onChange={(e) => setNetworkId(e.target.value)}>
            {(networks.data ?? []).map((n) => (
              <option key={n.id} value={n.id}>
                {n.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="Security groups" help="No group leaves the NIC unfiltered.">
          <div style={{ display: "flex", flexDirection: "column", gap: 7 }}>
            {(securityGroups.data ?? []).map((g) => (
              <Check
                key={g.id}
                on={sgIds.includes(g.id)}
                label={g.name}
                onChange={(on) =>
                  setSgIds((s) => (on ? [...s, g.id] : s.filter((x) => x !== g.id)))
                }
              />
            ))}
          </div>
        </Field>
        <div className="vq-help">
          The NIC re-homes without a restart. The guest keeps its IP, so on a different subnet renew
          DHCP or reconfigure it.
        </div>
        {change.isError && <ErrorPanel summary="Could not re-home the NIC" detail={change.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!networkId || change.isPending}
          onClick={() =>
            change.mutate(
              { id: vm.id, index, networkId, securityGroups: sgIds },
              { onSuccess: onClose },
            )
          }
        >
          Change
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function ConfirmDelete({ vm, onClose }: { vm: Vm; onClose: () => void }) {
  const action = useVmAction();
  const navigate = useNavigate();
  const [typed, setTyped] = useState("");
  return (
    <Dialog open onClose={onClose} maxWidth="xs" fullWidth>
      <DialogHead>Delete {vm.name}</DialogHead>
      <DialogBody>
        <div className="vq-help">
          This removes the VM and its desired state. Type its name to confirm.
        </div>
        <Input value={typed} autoFocus onChange={(e) => setTyped(e.target.value)} />
        {action.isError && <ErrorPanel summary="Delete failed" detail={action.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="destructive"
          disabled={typed !== vm.name || action.isPending}
          onClick={() =>
            action.mutate({ id: vm.id, action: "delete" }, { onSuccess: () => navigate("/vms") })
          }
        >
          Delete
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function MigrationBanner({ vm }: { vm: Vm }) {
  const tasks = useTasks();
  const task = (tasks.data ?? []).find(
    (t) => t.vm_id === vm.id && t.task_type.includes("migrate") && t.state === "Running",
  );
  return (
    <div
      style={{
        background: "var(--vq-cyan-soft)",
        border: "1px solid var(--vq-cyan-line)",
        borderRadius: "var(--vq-radius-card)",
        padding: "16px 18px",
        display: "flex",
        alignItems: "center",
        gap: 18,
      }}
    >
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ display: "flex", alignItems: "baseline", gap: 10 }}>
          <span style={{ fontSize: 13.5, fontWeight: 600, color: "var(--vq-cyan)" }}>
            Live migration in progress
          </span>
          <span className="vq-card-note">
            {task ? `task ${task.id.slice(0, 12)} · ${task.message ?? "transferring"}` : "starting"}
          </span>
        </div>
        <div style={{ margin: "10px 0" }}>
          <div className="vq-bar thick">
            <span className="vq-bar-cyan" style={{ width: `${task?.progress ?? 5}%` }} />
          </div>
        </div>
        <div
          className="vq-mono-sm"
          style={{ display: "flex", gap: 22, color: "var(--vq-text-2)", flexWrap: "wrap" }}
        >
          <span>{task ? `${task.progress}%` : "pre-copy"}</span>
          <span>{formatMib(vm.spec.memory.size_mib)} working set</span>
          <span>generation {vm.generation}</span>
        </div>
      </div>
    </div>
  );
}

export function VmDetail() {
  const { id } = useParams();
  const vm = useVm(id);
  const action = useVmAction();
  const metrics = useVmMetrics(id);
  const networks = useNetworks();
  const volumes = useVolumes();
  const hosts = useHosts();
  const tasks = useTasks();
  const events = useEvents();
  const { can } = usePermissions();
  const [params, setParams] = useSearchParams();
  const [editOpen, setEditOpen] = useState(false);
  const [migrateOpen, setMigrateOpen] = useState(false);
  const [nicIdx, setNicIdx] = useState<number | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);

  useCrumb(vm.data?.name);

  const m = metrics.data;
  const cpuHistory = useHistory(m?.running ? m.cpu_pct : undefined);
  const txHistory = useHistory(m?.running ? m.net_tx_bytes : undefined);

  const tab = ((params.get("tab") as Tab) ?? "Overview") as Tab;
  const setTab = (t: Tab) => {
    const next = new URLSearchParams(params);
    if (t === "Overview") next.delete("tab");
    else next.set("tab", t);
    setParams(next, { replace: true });
  };

  if (vm.isLoading) {
    return (
      <Table>
        <SkeletonRows cols="1.5fr 1fr 1fr 1fr" />
      </Table>
    );
  }
  if (vm.isError) return <QueryError error={vm.error} what="this VM" />;
  if (!vm.data) return <EmptyState headline="VM not found" hint="It may have been deleted." />;

  const v = vm.data;
  const networkName = (nid: string) => networks.data?.find((n) => n.id === nid)?.name ?? nid.slice(0, 8);
  const hostName = v.host_id
    ? (hosts.data?.find((h) => h.id === v.host_id)?.name ?? v.host_id.slice(0, 8))
    : null;
  const attached = (volumes.data ?? []).filter((vol) => vol.attached_vm_id === v.id);
  const vmTasks = (tasks.data ?? []).filter((t) => t.vm_id === v.id);
  const vmEvents = (events.data ?? []).filter((e) => e.resource_id === v.id);
  const boot =
    v.spec.boot.type === "direct_kernel"
      ? `direct kernel · ${v.spec.boot.kernel.split("/").pop()}`
      : `firmware · ${v.spec.boot.firmware.split("/").pop()}`;

  const interfaces = (
    <Card title="Interfaces">
      <Table>
        <THead cols={NIC_COLS}>
          <div>Network</div>
          <div>Address</div>
          <div>MAC</div>
          <div>Groups</div>
        </THead>
        {v.spec.network_interfaces.length === 0 && (
          <div style={{ padding: 18 }}>
            <EmptyState headline="No interfaces" hint="Add a NIC from Edit to give this VM a network." />
          </div>
        )}
        {v.spec.network_interfaces.map((nic, i) => (
          <TRow key={i} cols={NIC_COLS} onClick={can(ACTION.vmUpdate) ? () => setNicIdx(i) : undefined}>
            <div className="vq-cell">
              <Link className="vq-name" to="/networks">
                {networkName(nic.network_id)}
              </Link>
            </div>
            <div className="vq-cell vq-mono-sm">
              {nic.addresses?.[0] ?? (i === 0 ? (v.ip_address ?? <Dash />) : <Dash />)}
            </div>
            <div className="vq-cell vq-mono-sm">{nic.mac ?? <Dash />}</div>
            <div className="vq-cell vq-mono-sm">
              {nic.security_groups?.length ? nic.security_groups.length + " attached" : <Dash />}
            </div>
          </TRow>
        ))}
      </Table>
    </Card>
  );

  const volumeTable = (
    <Card title="Volumes">
      <Table>
        <THead cols={VOL_COLS}>
          <div>Volume</div>
          <div>Size</div>
          <div>Format</div>
          <div>Serial</div>
        </THead>
        {attached.length === 0 && (
          <div style={{ padding: 18 }}>
            <EmptyState headline="No managed volumes" hint="Disks declared by path appear under Spec." />
          </div>
        )}
        {attached.map((vol) => (
          <TRow key={vol.id} cols={VOL_COLS}>
            <div className="vq-cell">
              <Link className="vq-name" to="/volumes">
                {vol.name}
              </Link>
            </div>
            <div className="vq-mono-sm">{formatBytes(vol.size_bytes)}</div>
            <div className="vq-mono-sm">{vol.format}</div>
            <div className="vq-mono-sm">{vol.attached_serial ?? <Dash />}</div>
          </TRow>
        ))}
      </Table>
    </Card>
  );

  const specCard = (
    <Card title="Specification">
      <KV k="Machine type" v={v.spec.machine_type === "microvm" ? "microvm" : "standard"} />
      <KV k="Boot" v={boot} />
      <KV k="vCPU" v={`${v.spec.cpu.boot_vcpus} boot / ${v.spec.cpu.max_vcpus} max`} />
      <KV
        k="Memory"
        v={`${formatMib(v.spec.memory.size_mib)}${
          v.spec.memory.max_size_mib ? ` / ${formatMib(v.spec.memory.max_size_mib)} max` : ""
        }`}
      />
      <KV k="Placement" v={v.spec.placement.host ? `pinned · ${v.spec.placement.host}` : "auto"} />
      <KV
        k="Cloud-init"
        v={
          v.spec.cloud_init
            ? `hostname ${v.spec.cloud_init.hostname ?? v.name} · ${
                v.spec.cloud_init.ssh_authorized_keys?.length ?? 0
              } keys`
            : "none"
        }
      />
      <KV
        k="Desired power"
        v={
          <span className={v.spec.desired_power_state === "Running" ? "t-green" : "t-3"}>
            {v.spec.desired_power_state}
          </span>
        }
      />
    </Card>
  );

  return (
    <>
      <PageHeader
        back={
          <Link to="/vms" className="vq-backlink">
            ← Virtual machines
          </Link>
        }
        title={v.name}
        chips={<StateChip value={v.phase} />}
        subline={`${v.id} · generation ${v.generation} · observed ${v.observed_generation}`}
        actions={
          <>
            {can(ACTION.vmConsole) && (
              <Link to={`/vms/${v.id}/console`}>
                <Btn>Console</Btn>
              </Link>
            )}
            {can(ACTION.vmUpdate) && <Btn onClick={() => setEditOpen(true)}>Edit</Btn>}
            {can(ACTION.vmMigrate) && (
              <Btn onClick={() => setMigrateOpen(true)} disabled={v.phase !== "Running"}>
                Migrate
              </Btn>
            )}
            {can(ACTION.vmPower) &&
              (() => {
                // A migrating guest is running — offering "Start" would be a lie.
                const up = v.phase === "Running" || v.phase === "Migrating";
                return (
                  <Btn
                    disabled={v.phase === "Migrating"}
                    onClick={() => action.mutate({ id: v.id, action: up ? "stop" : "start" })}
                  >
                    {up ? "Stop" : "Start"}
                  </Btn>
                );
              })()}
            {can(ACTION.vmDelete) && (
              <Btn kind="destructive" onClick={() => setDeleteOpen(true)}>
                Delete
              </Btn>
            )}
          </>
        }
      />

      <div className="vq-tabs">
        {TABS.map((t) => (
          <button key={t} className={`vq-tab${t === tab ? " on" : ""}`} onClick={() => setTab(t)}>
            {t}
          </button>
        ))}
      </div>

      {action.isError && <ErrorPanel summary="Action failed" detail={action.error} />}
      {v.message && v.phase === "Failed" && (
        <ErrorPanel summary={`${v.name} failed`} detail={v.message} />
      )}

      {v.phase === "Migrating" && <MigrationBanner vm={v} />}

      {tab === "Overview" && (
        <>
          <Grid cols="1fr 1fr 1fr" className="vq-split">
            <Card padded>
              <div className="vq-metric-label">CPU</div>
              <div className="vq-metric-value">
                <span>{m?.running ? m.cpu_pct.toFixed(0) : "—"}</span>
                <span className="vq-metric-unit">% of {v.spec.cpu.boot_vcpus} vCPU</span>
              </div>
              <div style={{ marginTop: 14 }}>
                <Spark values={cpuHistory} />
              </div>
            </Card>
            <Card padded>
              <div className="vq-metric-label">Memory</div>
              <div className="vq-metric-value">
                <span>{m?.running ? (m.mem_bytes / GIB).toFixed(1) : "—"}</span>
                <span className="vq-metric-unit">
                  GiB / {formatMib(v.spec.memory.size_mib)}
                </span>
              </div>
              <div style={{ marginTop: 16 }}>
                <div className="vq-bar fat">
                  <span
                    className="vq-bar-blue"
                    style={{
                      width: `${
                        m?.running
                          ? Math.min(100, (m.mem_bytes / (v.spec.memory.size_mib * 1024 * 1024)) * 100)
                          : 0
                      }%`,
                    }}
                  />
                </div>
              </div>
            </Card>
            <Card padded>
              <div className="vq-metric-label">Throughput</div>
              <div style={{ display: "flex", gap: 26 }}>
                <div>
                  <div style={{ fontSize: 16, fontWeight: 600 }}>
                    {m?.running ? formatBytes(m.disk_write_bytes) : "—"}
                  </div>
                  <div className="vq-mono-sm" style={{ fontSize: 10 }}>
                    disk write
                  </div>
                </div>
                <div>
                  <div style={{ fontSize: 16, fontWeight: 600 }}>
                    {m?.running ? formatBytes(m.net_tx_bytes) : "—"}
                  </div>
                  <div className="vq-mono-sm" style={{ fontSize: 10 }}>
                    net tx
                  </div>
                </div>
              </div>
              <div style={{ marginTop: 12 }}>
                <Spark values={txHistory} cyan />
              </div>
            </Card>
          </Grid>
          <Grid cols="1fr 1fr" className="vq-split">
            {specCard}
            <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
              {interfaces}
              {volumeTable}
            </div>
          </Grid>
        </>
      )}

      {tab === "Spec" && (
        <Grid cols="1fr 1fr" className="vq-split">
          {specCard}
          <Card title="Disks">
            {v.spec.disks.length === 0 && (
              <div style={{ padding: 18 }}>
                <EmptyState headline="Diskless" hint="This guest boots from kernel and initramfs." />
              </div>
            )}
            {v.spec.disks.map((d, i) => (
              <KV
                key={i}
                labelWidth={90}
                k={`disk ${i}`}
                v={
                  <>
                    {d.path || "(auto-assigned)"}
                    {d.readonly && <span className="t-3"> · read-only</span>}
                    {d.size_bytes && <span className="t-3"> · {formatBytes(d.size_bytes)}</span>}
                  </>
                }
              />
            ))}
            <KV
              labelWidth={90}
              k="host"
              v={hostName ? <Link className="vq-name" to={`/hosts/${v.host_id}`}>{hostName}</Link> : "unscheduled"}
            />
          </Card>
        </Grid>
      )}

      {tab === "Networking" && interfaces}
      {tab === "Storage" && volumeTable}

      {tab === "Tasks" && (
        <Table>
          <THead cols={TASK_COLS}>
            <div>Task ID</div>
            <div>Type</div>
            <div>State</div>
            <div>Started</div>
            <div>Duration</div>
          </THead>
          {vmTasks.length === 0 && (
            <div style={{ padding: 18 }}>
              <EmptyState headline="No tasks for this VM" hint="Every mutating operation is recorded here." />
            </div>
          )}
          {vmTasks.map((t) => (
            <TRow key={t.id} cols={TASK_COLS}>
              <div className="vq-cell vq-mono-sm t-blue">{t.id.slice(0, 12)}</div>
              <div className="vq-cell vq-mono">{t.task_type}</div>
              <div>
                {t.state === "Running" ? (
                  <ProgressCell pct={t.progress} label={`${t.progress}%`} width={56} />
                ) : (
                  <StateChip value={t.state} dense />
                )}
              </div>
              <div className="vq-mono-sm">{formatTime(t.created_at)}</div>
              <div className="vq-mono-sm">{duration(t.created_at, t.updated_at)}</div>
            </TRow>
          ))}
        </Table>
      )}

      {tab === "Events" && (
        <Card title="Events">
          {vmEvents.length === 0 && (
            <div style={{ padding: 18 }}>
              <EmptyState headline="No events for this VM" />
            </div>
          )}
          {vmEvents.slice(0, 40).map((e) => (
            <div key={e.id} className="vq-eventrow">
              <span
                className="sev"
                style={{
                  background:
                    e.severity === "error"
                      ? "var(--vq-red)"
                      : e.severity === "warning"
                        ? "var(--vq-amber)"
                        : "var(--vq-cyan)",
                }}
              />
              <div>
                <div className="msg">{e.message}</div>
                <div className="meta">
                  {formatTime(e.ts, true)} · {e.event_type}
                </div>
              </div>
            </div>
          ))}
        </Card>
      )}

      {editOpen && <EditVmDialog vm={v} onClose={() => setEditOpen(false)} />}
      {migrateOpen && <MigrateDialog vm={v} onClose={() => setMigrateOpen(false)} />}
      {nicIdx != null && <ChangeNicDialog vm={v} index={nicIdx} onClose={() => setNicIdx(null)} />}
      {deleteOpen && <ConfirmDelete vm={v} onClose={() => setDeleteOpen(false)} />}
    </>
  );
}
