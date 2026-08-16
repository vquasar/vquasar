export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  // One decimal only when it carries information: "8 GiB", not "8.0 GiB".
  const dp = v >= 10 || i === 0 || Number.isInteger(v) ? 0 : 1;
  return `${v.toFixed(dp)} ${units[i]}`;
}

export function formatMib(mib: number): string {
  return formatBytes(mib * 1024 * 1024);
}

export function formatDate(iso: string | null | undefined): string {
  if (!iso) return "—";
  const d = new Date(iso);
  return d.toLocaleString();
}

export function shortId(id: string | null | undefined): string {
  return id ? id.slice(0, 8) : "—";
}

/// Wall-clock time of day. The console's tables are logs — alignment matters
/// more than locale niceties, so this is fixed-width.
export function formatTime(iso: string | null | undefined, millis = false): string {
  if (!iso) return "—";
  const d = new Date(iso);
  const pad = (n: number, w = 2) => String(n).padStart(w, "0");
  const base = `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  return millis ? `${base}.${pad(d.getMilliseconds(), 3)}` : base;
}

/// "3s ago" / "4m ago" / "2h ago". Returns null for a missing timestamp so the
/// caller can render a dash.
export function relTime(iso: string | null | undefined): string | null {
  if (!iso) return null;
  const secs = Math.max(0, Math.round((Date.now() - new Date(iso).getTime()) / 1000));
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`;
  return `${Math.floor(secs / 86400)}d ago`;
}

/// Seconds since a timestamp, for threshold checks (heartbeat staleness).
export function ageSecs(iso: string | null | undefined): number | null {
  if (!iso) return null;
  return Math.max(0, Math.round((Date.now() - new Date(iso).getTime()) / 1000));
}

/// Elapsed time between two timestamps, e.g. a task's duration: "1m 12s".
export function duration(from: string, to: string): string {
  const secs = Math.max(0, Math.round((new Date(to).getTime() - new Date(from).getTime()) / 1000));
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  if (m < 60) return `${m}m ${s}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/// Two-letter avatar initials from a username like "d.kaur" or "Deepa Kaur".
export function initials(name: string): string {
  const parts = name.split(/[.\s_@-]+/).filter(Boolean);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return name.slice(0, 2).toUpperCase();
}

/// How uniform the fleet is on one piece of software.
///
/// Three outcomes are worth telling apart, and the third is why this exists:
/// everyone agrees, they disagree, or some host has not said. A host that has
/// not reported is *not* evidence of agreement, so it is counted rather than
/// filtered away — silently dropping it is what lets a host sit versions
/// behind the rest with nothing on screen suggesting so.
export interface FleetVersions {
  /// Distinct versions actually reported, sorted so the order is stable
  /// between renders rather than following host order.
  known: string[];
  /// Hosts that reported nothing.
  unknown: number;
}

export function fleetVersions(values: (string | null | undefined)[]): FleetVersions {
  return {
    known: [...new Set(values.filter((v): v is string => !!v))].sort(),
    unknown: values.filter((v) => !v).length,
  };
}

/// The same thing as one line of prose: "cloud-hypervisor v53.0",
/// "2 agent versions", "agent v1, 1 unknown".
export function fleetSummary(f: FleetVersions, what: string): string {
  const head =
    f.known.length === 0
      ? `${what} unknown`
      : f.known.length === 1
        ? `${what} ${f.known[0]}`
        : `${f.known.length} ${what} versions`;
  // An unknown alongside known ones is the interesting case: it reads as
  // agreement otherwise, which is the failure this whole helper is about.
  return f.known.length > 0 && f.unknown > 0 ? `${head}, ${f.unknown} unknown` : head;
}

/// "12s ago" / "4m ago" / "2h ago" — for a liveness signal, where the exact
/// timestamp matters less than whether it is recent.
export function formatRelative(iso: string | null | undefined): string {
  if (!iso) return "—";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "—";
  const secs = Math.max(0, Math.round((Date.now() - then) / 1000));
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.round(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.round(secs / 3600)}h ago`;
  return `${Math.round(secs / 86400)}d ago`;
}
