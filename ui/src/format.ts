export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 10 || i === 0 ? 0 : 1)} ${units[i]}`;
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
