// Virtual machines (handoff §4). Filter state lives in the URL so a filtered
// view is linkable; selection and paging are local.

import { useMemo, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useHosts, useVmAction, useVms } from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import {
  Btn,
  Dash,
  EmptyState,
  ErrorPanel,
  Pagination,
  PageHeader,
  QueryError,
  RowMenu,
  Segmented,
  SkeletonRows,
  StateChip,
  Table,
  THead,
  TRow,
} from "../ui/kit";
import { formatMib, shortId } from "../format";
import type { Vm } from "../api/types";

const COLS = "26px 1.6fr 130px 1fr 70px 100px 120px 1fr 40px";
const PAGE_SIZE = 20;

type Filter = "all" | "running" | "stopped" | "migrating";

/// The image a VM booted from, as far as its spec reveals: the first writable
/// disk's source, else its path.
function imageLabel(v: Vm): string | null {
  const disk = v.spec.disks.find((d) => !d.readonly) ?? v.spec.disks[0];
  const p = disk?.source || disk?.path;
  if (!p) return null;
  return p.split("/").pop() ?? p;
}

export function Vms() {
  const vms = useVms();
  const hosts = useHosts();
  const action = useVmAction();
  const navigate = useNavigate();
  const { can } = usePermissions();
  const [params, setParams] = useSearchParams();
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [page, setPage] = useState(1);

  const q = params.get("q") ?? "";
  const filter = (params.get("state") as Filter) ?? "all";
  const setParam = (k: string, v: string) => {
    const next = new URLSearchParams(params);
    if (v && v !== "all") next.set(k, v);
    else next.delete(k);
    setParams(next, { replace: true });
    setPage(1);
  };

  const hostName = useMemo(() => {
    const m = new Map<string, string>();
    (hosts.data ?? []).forEach((h) => m.set(h.id, h.name));
    return m;
  }, [hosts.data]);

  const list = vms.data ?? [];
  const counts = {
    all: list.length,
    running: list.filter((v) => v.phase === "Running").length,
    stopped: list.filter((v) => v.phase === "Stopped").length,
    migrating: list.filter((v) => v.phase === "Migrating").length,
  };

  const filtered = list.filter((v) => {
    if (filter === "running" && v.phase !== "Running") return false;
    if (filter === "stopped" && v.phase !== "Stopped") return false;
    if (filter === "migrating" && v.phase !== "Migrating") return false;
    if (!q) return true;
    const needle = q.toLowerCase();
    return (
      v.name.toLowerCase().includes(needle) ||
      (v.ip_address ?? "").includes(needle) ||
      (v.host_id ? (hostName.get(v.host_id) ?? "").toLowerCase().includes(needle) : false)
    );
  });

  const pages = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const shown = filtered.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE);

  const toggle = (id: string) =>
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const allShownSelected = shown.length > 0 && shown.every((v) => selected.has(v.id));

  return (
    <>
      <PageHeader
        title="Virtual machines"
        subtitle={`${list.length} VM${list.length === 1 ? "" : "s"} across ${
          hosts.data?.length ?? 0
        } host${hosts.data?.length === 1 ? "" : "s"}${
          counts.migrating ? ` · ${counts.migrating} migrating` : ""
        }`}
        actions={
          can("vm:create") && (
            <>
              <Btn onClick={() => navigate("/templates")}>From template</Btn>
              <Btn kind="primary" onClick={() => navigate("/vms/new")}>
                Create VM
              </Btn>
            </>
          )
        }
      />

      <div className="vq-filterbar">
        <input
          className="vq-search"
          placeholder="Filter by name, IP, host…"
          value={q}
          onChange={(e) => setParam("q", e.target.value)}
        />
        <Segmented
          value={filter}
          onChange={(v) => setParam("state", v)}
          options={[
            { value: "all", label: `All ${counts.all}` },
            { value: "running", label: `Running ${counts.running}` },
            { value: "stopped", label: `Stopped ${counts.stopped}` },
            // Migrating stays cyan even when the segment is inactive: it is the
            // one state that means work is happening right now.
            { value: "migrating", label: `Migrating ${counts.migrating}`, tone: "cyan" },
          ]}
        />
      </div>

      <QueryError error={vms.error} what="virtual machines" />
      {action.isError && <ErrorPanel summary="Action failed" detail={action.error} />}

      <Table>
        <THead cols={COLS}>
          <button
            className={`vq-selectbox${allShownSelected ? " on" : ""}`}
            aria-label="Select all"
            onClick={() =>
              setSelected(allShownSelected ? new Set() : new Set(shown.map((v) => v.id)))
            }
          />
          <div>Name</div>
          <div>State</div>
          <div>Host</div>
          <div>vCPU</div>
          <div>Memory</div>
          <div>IP</div>
          <div>Image</div>
          <div />
        </THead>

        {vms.isLoading && <SkeletonRows cols={COLS} />}

        {!vms.isLoading && shown.length === 0 && (
          <div style={{ padding: 18 }}>
            <EmptyState
              headline={list.length ? "Nothing matches this filter" : "No virtual machines yet"}
              hint={
                list.length
                  ? "Clear the search or pick a different state."
                  : "Create one from a template, or define a spec by hand."
              }
            />
          </div>
        )}

        {shown.map((v) => {
          const image = imageLabel(v);
          const menu = [
            ...(can("vm:power")
              ? [
                  { label: "Start", onClick: () => action.mutate({ id: v.id, action: "start" }) },
                  { label: "Stop", onClick: () => action.mutate({ id: v.id, action: "stop" }) },
                ]
              : []),
            ...(can("vm:migrate") ? [{ label: "Migrate…", onClick: () => navigate(`/vms/${v.id}`) }] : []),
            ...(can("vm:delete")
              ? [
                  {
                    label: "Delete",
                    danger: true,
                    onClick: () => action.mutate({ id: v.id, action: "delete" }),
                  },
                ]
              : []),
          ];

          return (
            <TRow key={v.id} cols={COLS} tint={v.phase === "Migrating" ? "cyan" : undefined}>
              <button
                className={`vq-selectbox${selected.has(v.id) ? " on" : ""}`}
                aria-label={`Select ${v.name}`}
                onClick={() => toggle(v.id)}
              />
              <div className="vq-cell">
                <Link className="vq-name" to={`/vms/${v.id}`}>
                  {v.name}
                </Link>
              </div>
              <div>
                <StateChip value={v.phase} dense />
              </div>
              <div className="vq-cell vq-mono-sm">
                {v.host_id ? (hostName.get(v.host_id) ?? shortId(v.host_id)) : <Dash />}
              </div>
              <div className="vq-mono-sm">{v.spec.cpu.boot_vcpus}</div>
              <div className="vq-mono-sm">{formatMib(v.spec.memory.size_mib)}</div>
              <div className="vq-cell vq-mono-sm">{v.ip_address ?? <Dash />}</div>
              <div className="vq-cell vq-mono-sm">{image ?? <Dash />}</div>
              <RowMenu items={menu} />
            </TRow>
          );
        })}

        {filtered.length > 0 && (
          <Pagination
            page={page}
            pages={pages}
            shown={shown.length}
            total={filtered.length}
            onPage={setPage}
          />
        )}
      </Table>
    </>
  );
}
