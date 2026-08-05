// Events (handoff §12). The whole table is monospace on purpose: this is a log,
// and column alignment matters more than warmth.

import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useEvents, useHosts, useVms } from "../api/hooks";
import {
  Dash,
  EmptyState,
  QueryError,
  Segmented,
  SkeletonRows,
  Table,
  THead,
  TRow,
} from "../ui/kit";
import { formatTime } from "../format";

const COLS = "130px 90px 1.3fr 1.2fr 2.6fr";

type Severity = "all" | "info" | "warning" | "error";

function severityColor(sev: string): string {
  switch (sev) {
    case "error":
      return "var(--vq-red)";
    case "warning":
      return "var(--vq-amber)";
    default:
      return "var(--vq-cyan)";
  }
}

export function Events() {
  const events = useEvents();
  const vms = useVms();
  const hosts = useHosts();
  const [severity, setSeverity] = useState<Severity>("all");

  // Resolve a resource id to the name an operator recognises, and to a link
  // where one exists.
  const resolve = useMemo(() => {
    const m = new Map<string, { name: string; to?: string }>();
    (vms.data ?? []).forEach((v) => m.set(v.id, { name: v.name, to: `/vms/${v.id}` }));
    (hosts.data ?? []).forEach((h) => m.set(h.id, { name: h.name, to: `/hosts/${h.id}` }));
    return m;
  }, [vms.data, hosts.data]);

  const list = (events.data ?? []).filter((e) => severity === "all" || e.severity === severity);

  return (
    <>
      <div className="vq-pagehead">
        <div>
          <h1 className="vq-title">Events</h1>
          <div className="vq-sub">Append-only audit stream · retained 90 days</div>
        </div>
        <Segmented
          value={severity}
          onChange={setSeverity}
          options={[
            { value: "all", label: "All" },
            { value: "info", label: "Info" },
            { value: "warning", label: "Warning" },
            { value: "error", label: "Error" },
          ]}
        />
      </div>

      <QueryError error={events.error} what="events" />

      <Table>
        <THead cols={COLS}>
          <div>Timestamp</div>
          <div>Severity</div>
          <div>Event type</div>
          <div>Resource</div>
          <div>Message</div>
        </THead>

        {events.isLoading && <SkeletonRows cols={COLS} />}

        {!events.isLoading && list.length === 0 && (
          <div style={{ padding: 18 }}>
            <EmptyState
              headline={severity === "all" ? "No events yet" : `No ${severity} events`}
              hint="Every state change the control plane makes is recorded here."
            />
          </div>
        )}

        {list.map((e) => {
          const res = e.resource_id ? resolve.get(e.resource_id) : undefined;
          const isError = e.severity === "error";
          return (
            <TRow key={e.id} cols={COLS} gap={14} tint={isError ? "red" : undefined}>
              <div className="vq-mono-sm">{formatTime(e.ts, true)}</div>
              {/* Bare mono word, not a chip: five chips a row would drown the
                  message column. */}
              <div className="vq-mono-sm" style={{ color: severityColor(e.severity) }}>
                {e.severity}
              </div>
              <div className="vq-cell vq-mono">{e.event_type}</div>
              <div className="vq-cell vq-mono-sm">
                {res?.to ? (
                  <Link className="vq-name" to={res.to}>
                    {res.name}
                  </Link>
                ) : e.resource_id ? (
                  e.resource_id.slice(0, 8)
                ) : (
                  <Dash />
                )}
              </div>
              <div
                className="vq-cell vq-mono-sm"
                style={isError ? { color: "var(--vq-red)" } : undefined}
                title={e.message}
              >
                {e.message}
              </div>
            </TRow>
          );
        })}
      </Table>
    </>
  );
}
