// Overview — desired vs. observed state across the fleet, in one screen
// (handoff §1). Drift is a first-class metric here: a resource whose observed
// generation trails its desired one is the whole point of a reconciled system.

import { useMemo } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useEvents, useHosts, useNetworks, useTasks, useVms } from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { ACTION } from "../auth/perm";
import {
  Card,
  Dash,
  EmptyState,
  Grid,
  Metric,
  ProgressCell,
  QueryError,
  StateChip,
  Table,
  THead,
  TRow,
  Btn,
  SkeletonRows,
} from "../ui/kit";
import { formatBytes, formatTime, relTime } from "../format";
import type { Host } from "../api/types";

const TASK_COLS = "1.5fr 1fr 1fr 1.5fr";

function hostMemUsed(h: Host): { used: number; total: number; pct: number } | null {
  if (h.total_memory_bytes == null || h.available_memory_bytes == null) return null;
  const total = h.total_memory_bytes;
  const used = Math.max(0, total - h.available_memory_bytes);
  return { used, total, pct: total > 0 ? (used / total) * 100 : 0 };
}

function severityColor(sev: string): string {
  switch (sev) {
    case "error":
      return "var(--vq-red)";
    case "warning":
      return "var(--vq-amber)";
    case "info":
      return "var(--vq-cyan)";
    default:
      return "var(--vq-text-4)";
  }
}

export function Overview() {
  const hosts = useHosts();
  const vms = useVms();
  const tasks = useTasks();
  const events = useEvents();
  const networks = useNetworks();
  const navigate = useNavigate();
  const { can } = usePermissions();

  const hostList = hosts.data ?? [];
  const vmList = vms.data ?? [];
  const taskList = tasks.data ?? [];

  const hostName = useMemo(() => {
    const m = new Map<string, string>();
    hostList.forEach((h) => m.set(h.id, h.name));
    return m;
  }, [hostList]);
  const vmById = useMemo(() => {
    const m = new Map<string, (typeof vmList)[number]>();
    vmList.forEach((v) => m.set(v.id, v));
    return m;
  }, [vmList]);

  const ready = hostList.filter((h) => h.state === "Ready").length;
  const cordoned = hostList.filter((h) => !h.schedulable);
  const running = vmList.filter((v) => v.phase === "Running").length;
  const stopped = vmList.filter((v) => v.phase === "Stopped").length;
  const failed = vmList.filter((v) => v.phase === "Failed").length;
  const migrating = vmList.filter((v) => v.phase === "Migrating");
  const drifting = vmList.filter((v) => v.observed_generation !== v.generation);
  const active = taskList.filter((t) => t.state === "Pending" || t.state === "Running");

  // Fleet memory. vCPU allocation counts what the VMs asked for against what
  // the hosts actually have.
  const mem = hostList.reduce(
    (acc, h) => {
      const m = hostMemUsed(h);
      return m ? { used: acc.used + m.used, total: acc.total + m.total } : acc;
    },
    { used: 0, total: 0 },
  );
  const memPct = mem.total > 0 ? Math.round((mem.used / mem.total) * 100) : 0;
  const logicalCpus = hostList.reduce((n, h) => n + (h.logical_cpus ?? 0), 0);
  const allocatedVcpus = vmList
    .filter((v) => v.phase !== "Stopped")
    .reduce((n, v) => n + v.spec.cpu.boot_vcpus, 0);
  const vcpuPct = logicalCpus > 0 ? Math.round((allocatedVcpus / logicalCpus) * 100) : 0;

  // The eight most-loaded hosts; the rest live on /hosts.
  const busiest = [...hostList]
    .map((h) => ({ h, m: hostMemUsed(h) }))
    .sort((a, b) => (b.m?.pct ?? -1) - (a.m?.pct ?? -1))
    .slice(0, 8);
  const arrivingOn = new Set(
    migrating.map((v) => v.host_id).filter((id): id is string => !!id),
  );

  const lastHeartbeat = hostList
    .map((h) => h.last_heartbeat)
    .filter((t): t is string => !!t)
    .sort()
    .at(-1);

  return (
    <>
      <div className="vq-pagehead">
        <div>
          <h1 className="vq-title">Overview</h1>
          <div className="vq-sub">
            Desired and observed state across {hostList.length} host
            {hostList.length === 1 ? "" : "s"}.{" "}
            {lastHeartbeat
              ? `Last agent heartbeat ${relTime(lastHeartbeat)}.`
              : "No agent has reported yet."}
          </div>
        </div>
        <div className="vq-actions">
          <Btn onClick={() => navigate("/tasks")}>View tasks</Btn>
          {can(ACTION.vmCreate) && (
            <Btn kind="primary" onClick={() => navigate("/vms/new")}>
              Create VM
            </Btn>
          )}
        </div>
      </div>

      <QueryError error={hosts.error} what="hosts" />
      <QueryError error={vms.error} what="virtual machines" />

      <Grid cols="repeat(5, 1fr)" className="vq-metrics-5">
        <Metric
          label="Hosts ready"
          value={ready}
          unit={`/ ${hostList.length}`}
          bar={[
            { pct: hostList.length ? (ready / hostList.length) * 100 : 0, tone: "green" },
            {
              pct: hostList.length ? ((hostList.length - ready) / hostList.length) * 100 : 0,
              tone: "amber",
            },
          ]}
          caption={
            cordoned.length
              ? `${cordoned.map((h) => h.name).slice(0, 2).join(", ")}${
                  cordoned.length > 2 ? ` +${cordoned.length - 2}` : ""
                } cordoned`
              : "all hosts schedulable"
          }
        />
        <Metric
          label="VMs running"
          value={running}
          unit={`/ ${vmList.length}`}
          bar={[
            { pct: vmList.length ? (running / vmList.length) * 100 : 0, tone: "blue" },
            {
              pct: vmList.length ? ((vmList.length - running) / vmList.length) * 100 : 0,
              tone: "inert",
            },
          ]}
          caption={`${stopped} stopped · ${failed} failed`}
        />
        <Metric
          label="Live migrations"
          value={migrating.length}
          unit="in flight"
          tone={migrating.length ? "cyan" : undefined}
          bar={[{ pct: migrating.length ? 100 : 0, tone: "cyan" }]}
          caption={
            migrating.length
              ? `${migrating.map((v) => v.name).slice(0, 2).join(", ")}${
                  migrating.length > 2 ? ` +${migrating.length - 2}` : ""
                }`
              : "nothing moving"
          }
          captionTone={migrating.length ? "cyan" : undefined}
        />
        <Metric
          label="Capacity used"
          value={memPct}
          unit="% memory"
          bar={[{ pct: memPct, tone: "blue" }]}
          caption={`${formatBytes(mem.used)} / ${formatBytes(mem.total)} · ${vcpuPct}% vCPU`}
        />
        <Metric
          label="Drift"
          value={drifting.length}
          unit={drifting.length === 1 ? "resource" : "resources"}
          tone={drifting.length ? "amber" : undefined}
          bar={[{ pct: drifting.length ? 100 : 0, tone: "amber" }]}
          caption={
            drifting.length ? `${drifting[0].name} awaiting apply` : "observed matches desired"
          }
          captionTone={drifting.length ? "amber" : undefined}
        />
      </Grid>

      <Grid cols="1.5fr 1fr" className="vq-split">
        <Card title="Active tasks" note="persisted state machines">
          <Table>
            <THead cols={TASK_COLS}>
              <div>Task</div>
              <div>Resource</div>
              <div>Placement</div>
              <div>State</div>
            </THead>
            {tasks.isLoading && <SkeletonRows cols={TASK_COLS} rows={4} />}
            {!tasks.isLoading && active.length === 0 && (
              <div style={{ padding: 18 }}>
                <EmptyState
                  headline="Nothing in flight"
                  hint="Every mutating operation appears here while it runs."
                />
              </div>
            )}
            {active.map((t) => {
              const vm = t.vm_id ? vmById.get(t.vm_id) : undefined;
              const placement = vm?.host_id ? hostName.get(vm.host_id) ?? vm.host_id.slice(0, 8) : null;
              return (
                <TRow key={t.id} cols={TASK_COLS}>
                  <div className="vq-cell vq-mono">{t.task_type}</div>
                  <div className="vq-cell">
                    {vm ? (
                      <Link className="vq-name" to={`/vms/${vm.id}`}>
                        {vm.name}
                      </Link>
                    ) : (
                      <Dash />
                    )}
                  </div>
                  <div className="vq-cell vq-mono-sm">{placement ?? "unscheduled"}</div>
                  <div>
                    {t.state === "Running" ? (
                      <ProgressCell pct={t.progress} label={`${t.progress}%`} />
                    ) : (
                      <StateChip value={t.state} dense />
                    )}
                  </div>
                </TRow>
              );
            })}
          </Table>
        </Card>

        <Card
          title="Events"
          actions={
            <Link to="/events" className="vq-card-note" style={{ color: "var(--vq-blue)" }}>
              View all
            </Link>
          }
        >
          {events.isLoading && <SkeletonRows cols="1fr" rows={5} />}
          {(events.data ?? []).slice(0, 5).map((e) => (
            <div key={e.id} className="vq-eventrow">
              <span className="sev" style={{ background: severityColor(e.severity) }} />
              <div style={{ minWidth: 0 }}>
                <div className="msg">{e.message}</div>
                <div className="meta">
                  {formatTime(e.ts)} · {e.event_type}
                </div>
              </div>
            </div>
          ))}
          {!events.isLoading && (events.data ?? []).length === 0 && (
            <div style={{ padding: 18 }}>
              <EmptyState headline="No events yet" hint="The audit stream is append-only." />
            </div>
          )}
        </Card>
      </Grid>

      <Card
        title="Host capacity"
        note={
          <span className="vq-legend">
            <span>
              <i style={{ background: "var(--vq-blue)" }} />
              allocated
            </span>
            <span>
              <i style={{ background: "var(--vq-cyan)" }} />
              migrating in
            </span>
            <span>
              <i style={{ background: "var(--vq-surface-3)" }} />
              free
            </span>
          </span>
        }
        padded
      >
        {busiest.length === 0 ? (
          <EmptyState
            headline="No hosts registered"
            hint="Enroll a host to give the scheduler somewhere to place VMs."
          />
        ) : (
          <Grid cols={`repeat(${Math.min(busiest.length, 8)}, 1fr)`} gap={10}>
            {busiest.map(({ h, m }) => {
              const pct = m?.pct ?? 0;
              const arriving = arrivingOn.has(h.id);
              return (
                <Link key={h.id} to={`/hosts/${h.id}`} className="vq-hostcol">
                  <div className="head">
                    <span
                      style={{
                        color: !h.schedulable
                          ? "var(--vq-amber)"
                          : arriving
                            ? "var(--vq-cyan)"
                            : "var(--vq-text-2)",
                      }}
                    >
                      {h.name}
                    </span>
                    <span className="t-4">{m ? `${Math.round(pct)}%` : "—"}</span>
                  </div>
                  <div className={`vq-hosttrack${!h.schedulable ? " cordoned" : ""}`}>
                    <span
                      style={{
                        height: `${Math.min(100, pct)}%`,
                        background: h.schedulable ? "var(--vq-blue)" : "var(--vq-amber)",
                        opacity: h.schedulable ? 1 : 0.7,
                      }}
                    />
                    {arriving && (
                      <span style={{ height: "12%", background: "var(--vq-cyan)" }} />
                    )}
                  </div>
                </Link>
              );
            })}
          </Grid>
        )}
      </Card>

      {networks.isError && <QueryError error={networks.error} what="networks" />}
    </>
  );
}
