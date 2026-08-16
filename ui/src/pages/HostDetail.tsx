// Host detail (handoff §3) — a new route. The migration-compatibility card is
// the reason it exists: live migration needs the target to expose a superset of
// the source's guest-visible ISA flags, and an operator should be able to see
// which hosts qualify before they try (design M15).

import { useMemo, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { useDrainHost, useHost, useHosts, useSetHostSchedulable, useVms } from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { ACTION } from "../auth/perm";
import { useCrumb } from "../components/Breadcrumb";
import {
  Btn,
  Card,
  Dash,
  EmptyState,
  ErrorPanel,
  Grid,
  KV,
  Metric,
  PageHeader,
  QueryError,
  SkeletonRows,
  StateChip,
  Table,
  THead,
  TRow,
} from "../ui/kit";
import { ageSecs, formatBytes, formatMib, relTime } from "../format";
import type { Host } from "../api/types";

const VM_COLS = "1.5fr 100px 1fr 1fr";

/// Which hosts this one can hand a running guest to. Purely advisory — the
/// scheduler enforces the same rule server-side.
function compatibility(source: Host, all: Host[]) {
  const targets = all.filter((h) => h.id !== source.id && h.state === "Ready");
  const srcFeatures = source.cpu_features ?? [];
  const compatible: Host[] = [];
  const blocked: { host: Host; reason: string }[] = [];

  for (const t of targets) {
    if (source.cpu_vendor && t.cpu_vendor && source.cpu_vendor !== t.cpu_vendor) {
      blocked.push({ host: t, reason: `vendor ${t.cpu_vendor}` });
      continue;
    }
    const have = new Set(t.cpu_features ?? []);
    const missing = srcFeatures.filter((f) => !have.has(f));
    if (missing.length) blocked.push({ host: t, reason: `missing ${missing.join(", ")}` });
    else compatible.push(t);
  }
  return { compatible, blocked };
}

/// Group blocked targets by reason so the card reads "9 hosts — missing vaes"
/// rather than nine near-identical lines.
function summariseBlocked(blocked: { host: Host; reason: string }[]): string[] {
  const byReason = new Map<string, number>();
  blocked.forEach((b) => byReason.set(b.reason, (byReason.get(b.reason) ?? 0) + 1));
  return [...byReason.entries()].map(([reason, n]) => `${n} host${n === 1 ? "" : "s"} — ${reason}`);
}

export function HostDetail() {
  const { id } = useParams();
  const host = useHost(id);
  const hosts = useHosts();
  const vms = useVms();
  const { can } = usePermissions();
  const setSchedulable = useSetHostSchedulable();
  const drain = useDrainHost();
  const [drained, setDrained] = useState<string | null>(null);

  useCrumb(host.data?.name);
  const h = host.data;
  const placed = useMemo(
    () => (vms.data ?? []).filter((v) => v.host_id === id),
    [vms.data, id],
  );

  if (host.isLoading) {
    return (
      <Table>
        <SkeletonRows cols={VM_COLS} />
      </Table>
    );
  }
  if (host.isError) return <QueryError error={host.error} what="this host" />;
  if (!h) return <EmptyState headline="Host not found" hint="It may have been removed." />;

  const total = h.total_memory_bytes;
  const avail = h.available_memory_bytes;
  const usedMem = total != null && avail != null ? total - avail : null;
  const memPct = usedMem != null && total ? (usedMem / total) * 100 : 0;

  const arriving = placed.filter((v) => v.phase === "Migrating");
  const running = placed.filter((v) => v.phase === "Running").length;
  const stopped = placed.filter((v) => v.phase === "Stopped").length;
  const allocatedVcpus = placed
    .filter((v) => v.phase !== "Stopped")
    .reduce((n, v) => n + v.spec.cpu.boot_vcpus, 0);

  const { compatible, blocked } = compatibility(h, hosts.data ?? []);
  const age = ageSecs(h.last_heartbeat);
  const heartbeatFresh = age != null && age <= 30;

  return (
    <>
      <PageHeader
        back={
          <Link to="/hosts" className="vq-backlink">
            ← Hosts
          </Link>
        }
        title={h.name}
        chips={
          <>
            <StateChip value={h.state} />
            {arriving.length > 0 && (
              <StateChip value="Receiving migration" tone="cyan" pulse />
            )}
          </>
        }
        subline={
          <>
            {h.endpoint} · mTLS
            {` · agent ${h.agent_version ?? "version unknown"}`}
            {h.cloud_hypervisor_version && ` · cloud-hypervisor ${h.cloud_hypervisor_version}`}
            {h.kernel_version && ` · ${h.kernel_version}`}
            {h.architecture && ` · ${h.architecture}`}
          </>
        }
        actions={
          can(ACTION.hostCordon) && (
            <>
              <Btn
                onClick={() => setSchedulable.mutate({ id: h.id, schedulable: !h.schedulable })}
                disabled={setSchedulable.isPending}
              >
                {h.schedulable ? "Cordon" : "Uncordon"}
              </Btn>
              <Btn
                kind="caution"
                disabled={drain.isPending}
                onClick={() =>
                  drain.mutate(h.id, {
                    onSuccess: (r) =>
                      setDrained(
                        `${r.migrating.length} migrating, ${r.skipped.length} left in place`,
                      ),
                  })
                }
              >
                Drain
              </Btn>
            </>
          )
        }
      />

      {drain.isError && <ErrorPanel summary="Drain failed" detail={drain.error} />}
      {drained && <div className="vq-warnpanel">Drain started — {drained}.</div>}

      <Grid cols="repeat(4, 1fr)" className="vq-metrics-4">
        <Metric
          label="Memory"
          value={usedMem != null ? formatBytes(usedMem).split(" ")[0] : "—"}
          unit={total != null ? `/ ${formatBytes(total)}` : undefined}
          bar={[
            { pct: memPct, tone: "blue" },
            { pct: arriving.length ? 6 : 0, tone: "cyan" },
          ]}
        />
        <Metric
          label="vCPU allocated"
          value={allocatedVcpus}
          unit={h.logical_cpus != null ? `/ ${h.logical_cpus} logical` : undefined}
          bar={[
            {
              pct: h.logical_cpus ? (allocatedVcpus / h.logical_cpus) * 100 : 0,
              tone: "blue",
            },
          ]}
        />
        <Metric
          label="VMs"
          value={placed.length}
          unit={arriving.length ? `+${arriving.length} arriving` : undefined}
          caption={`${running} running · ${stopped} stopped`}
        />
        <Metric
          label="Heartbeat"
          value={relTime(h.last_heartbeat)?.replace(" ago", "") ?? "—"}
          unit="ago"
          tone={heartbeatFresh ? "green" : "red"}
          caption={`generation ${h.generation} reconciled`}
        />
      </Grid>

      <Grid cols="1fr 1fr" className="vq-split">
        <Card title="Placed VMs">
          <Table>
            <THead cols={VM_COLS}>
              <div>VM</div>
              <div>State</div>
              <div>vCPU / mem</div>
              <div>IP</div>
            </THead>
            {vms.isLoading && <SkeletonRows cols={VM_COLS} rows={4} />}
            {!vms.isLoading && placed.length === 0 && (
              <div style={{ padding: 18 }}>
                <EmptyState headline="No VMs placed here" hint="The scheduler has not used this host yet." />
              </div>
            )}
            {placed.map((v) => {
              const isArriving = v.phase === "Migrating";
              const tone =
                v.phase === "Running"
                  ? "var(--vq-green)"
                  : isArriving
                    ? "var(--vq-cyan)"
                    : v.phase === "Failed"
                      ? "var(--vq-red)"
                      : "var(--vq-text-3)";
              return (
                <TRow key={v.id} cols={VM_COLS} tint={isArriving ? "cyan" : undefined}>
                  <div className="vq-cell">
                    <Link className="vq-name" to={`/vms/${v.id}`}>
                      {v.name}
                    </Link>
                  </div>
                  {/* Bare mono here, not a chip: this table is dense enough that
                      chips would out-shout the names. */}
                  <div className="vq-mono-sm" style={{ color: tone, fontSize: 10.5 }}>
                    {isArriving ? "Arriving" : v.phase}
                  </div>
                  <div className="vq-mono-sm">
                    {v.spec.cpu.boot_vcpus} / {formatMib(v.spec.memory.size_mib)}
                  </div>
                  <div className="vq-cell vq-mono-sm">
                    {v.ip_address ?? (isArriving ? "pending" : <Dash />)}
                  </div>
                </TRow>
              );
            })}
          </Table>
        </Card>

        <Card title="Migration compatibility">
          <KV k="CPU vendor" v={h.cpu_vendor ?? <Dash />} labelWidth={130} />
          <KV
            k="Guest ISA flags"
            labelWidth={130}
            v={
              (h.cpu_features ?? []).length ? (
                <span className="vq-pills">
                  {h.cpu_features.map((f) => (
                    <span key={f} className="vq-pill">
                      {f}
                    </span>
                  ))}
                </span>
              ) : (
                <Dash />
              )
            }
          />
          <KV
            k="Compatible targets"
            labelWidth={130}
            v={
              <span className="t-green">
                {compatible.length} host{compatible.length === 1 ? "" : "s"}
              </span>
            }
          />
          <KV
            k="Blocked targets"
            labelWidth={130}
            v={
              blocked.length ? (
                <span className="t-amber">{summariseBlocked(blocked).join(" · ")}</span>
              ) : (
                <span className="t-3">none</span>
              )
            }
          />
          <div style={{ padding: "14px 18px", borderTop: "1px solid var(--vq-line)" }}>
            <div style={{ fontFamily: "var(--vq-font-body)", fontSize: 12, color: "var(--vq-text-3)" }}>
              Live migration requires the target host to expose a superset of the guest-visible ISA
              flags. Cross-vendor moves are rejected by the scheduler.
            </div>
          </div>
        </Card>
      </Grid>
    </>
  );
}
