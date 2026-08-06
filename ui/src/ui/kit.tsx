// The recurring components. Every screen composes these; none of them carries
// a hex value of its own (handoff, "Recurring components").

import {
  useState,
  type CSSProperties,
  type InputHTMLAttributes,
  type ReactNode,
  type SelectHTMLAttributes,
} from "react";
import Menu from "@mui/material/Menu";
import MenuItem from "@mui/material/MenuItem";
import { MarkWatermark } from "./Mark";

/* ------------------------------------------------------------- semantics -- */

export type Tone = "green" | "cyan" | "amber" | "red" | "blue" | "inert";

/// The single mapping from a domain state to a colour. Cyan is reserved for
/// live, in-flight work — a pulsing dot is a promise that bytes are moving.
export function toneFor(value: string): { tone: Tone; pulse: boolean } {
  switch (value) {
    case "Running":
    case "Ready":
    case "Succeeded":
    case "ready":
      return { tone: "green", pulse: false };
    case "Migrating":
    case "Arriving":
    case "importing":
    case "snapshotting":
      return { tone: "cyan", pulse: true };
    case "Starting":
    case "Stopping":
    case "Creating":
    case "Deleting":
      return { tone: "cyan", pulse: false };
    case "Pending":
    case "Scheduling":
    case "Maintenance":
    case "Cordoned":
      return { tone: "amber", pulse: false };
    case "Failed":
    case "NotReady":
    case "failed":
      return { tone: "red", pulse: false };
    default:
      // Stopped, Disabled, Cancelled and anything inert.
      return { tone: "inert", pulse: false };
  }
}

/* ------------------------------------------------------------ state chip -- */

export function StateChip({
  value,
  tone,
  pulse,
  dense,
  title,
}: {
  value: string;
  tone?: Tone;
  pulse?: boolean;
  dense?: boolean;
  title?: string;
}) {
  const auto = toneFor(value);
  return (
    <span
      className={`vq-chip ${tone ?? auto.tone}${dense ? " dense" : ""}`}
      title={title}
    >
      <i className={`dot${(pulse ?? auto.pulse) ? " vq-pulse-fast" : ""}`} />
      {value}
    </span>
  );
}

/* ----------------------------------------------------------- page header -- */

export function PageHeader({
  title,
  subtitle,
  subline,
  back,
  actions,
  chips,
}: {
  title: ReactNode;
  subtitle?: ReactNode;
  subline?: ReactNode;
  back?: ReactNode;
  actions?: ReactNode;
  chips?: ReactNode;
}) {
  return (
    <div>
      {back}
      <div className="vq-pagehead">
        <div style={{ minWidth: 0 }}>
          <h1 className="vq-title">
            {title}
            {chips}
          </h1>
          {subtitle && <div className="vq-sub">{subtitle}</div>}
          {subline && <div className="vq-subline">{subline}</div>}
        </div>
        {actions && <div className="vq-actions">{actions}</div>}
      </div>
    </div>
  );
}

/* ------------------------------------------------------------------ card -- */

export function Card({
  title,
  note,
  desc,
  actions,
  children,
  style,
  padded,
}: {
  title?: ReactNode;
  note?: ReactNode;
  desc?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
  style?: CSSProperties;
  padded?: boolean;
}) {
  return (
    <div className="vq-card" style={style}>
      {(title || actions) && (
        <div className="vq-card-head">
          <div style={{ minWidth: 0 }}>
            <div className="vq-card-title">{title}</div>
            {desc && <div className="vq-card-desc">{desc}</div>}
          </div>
          {note && <div className="vq-card-note">{note}</div>}
          {actions}
        </div>
      )}
      {padded ? <div className="vq-card-body">{children}</div> : children}
    </div>
  );
}

export function Grid({
  cols,
  gap,
  className,
  children,
  style,
}: {
  cols: string;
  gap?: number;
  className?: string;
  children: ReactNode;
  style?: CSSProperties;
}) {
  return (
    <div
      className={`vq-grid${className ? ` ${className}` : ""}`}
      style={{ gridTemplateColumns: cols, ...(gap != null ? { gap } : {}), ...style }}
    >
      {children}
    </div>
  );
}

/* ----------------------------------------------------------------- table -- */

export function Table({ children, style }: { children: ReactNode; style?: CSSProperties }) {
  return (
    <div className="vq-table" style={style}>
      {children}
    </div>
  );
}

export function THead({ cols, children }: { cols: string; children: ReactNode }) {
  return (
    <div className="vq-th" style={{ gridTemplateColumns: cols }}>
      {children}
    </div>
  );
}

export function TRow({
  cols,
  tint,
  gap,
  children,
  onClick,
}: {
  cols: string;
  tint?: "amber" | "cyan" | "red";
  gap?: number;
  children: ReactNode;
  onClick?: () => void;
}) {
  return (
    <div
      className={`vq-tr${tint ? ` tint-${tint}` : ""}`}
      style={{ gridTemplateColumns: cols, ...(gap != null ? { gap } : {}) }}
      onClick={onClick}
    >
      {children}
    </div>
  );
}

/// An empty cell is a dash, not a blank.
export function Dash() {
  return <span className="vq-dash">—</span>;
}

export function Mono({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={`vq-mono${className ? ` ${className}` : ""}`}>{children}</span>;
}

export function TableFooter({ left, right }: { left: ReactNode; right?: ReactNode }) {
  return (
    <div className="vq-tfoot">
      <span>{left}</span>
      {right && <span style={{ display: "flex", gap: 8 }}>{right}</span>}
    </div>
  );
}

export function Pagination({
  page,
  pages,
  shown,
  total,
  onPage,
}: {
  page: number;
  pages: number;
  shown: number;
  total: number;
  onPage: (p: number) => void;
}) {
  return (
    <TableFooter
      left={`${shown} of ${total} · page ${page} / ${Math.max(pages, 1)}`}
      right={
        <>
          <button className="vq-btn" style={{ height: 24 }} disabled={page <= 1} onClick={() => onPage(page - 1)}>
            Prev
          </button>
          <button
            className="vq-btn"
            style={{ height: 24 }}
            disabled={page >= pages}
            onClick={() => onPage(page + 1)}
          >
            Next
          </button>
        </>
      }
    />
  );
}

/// The row overflow menu. Entries already filtered by permission at the call
/// site; an empty list renders nothing.
export function RowMenu({ items }: { items: { label: string; onClick: () => void; danger?: boolean }[] }) {
  const [anchor, setAnchor] = useState<null | HTMLElement>(null);
  if (items.length === 0) return <span />;
  return (
    <>
      <button
        className="vq-rowmenu"
        aria-label="Actions"
        onClick={(e) => {
          e.stopPropagation();
          setAnchor(e.currentTarget);
        }}
      >
        ···
      </button>
      <Menu anchorEl={anchor} open={!!anchor} onClose={() => setAnchor(null)}>
        {items.map((it) => (
          <MenuItem
            key={it.label}
            onClick={() => {
              setAnchor(null);
              it.onClick();
            }}
            sx={it.danger ? { color: "error.main" } : undefined}
          >
            {it.label}
          </MenuItem>
        ))}
      </Menu>
    </>
  );
}

/* -------------------------------------------------------------- progress -- */

export interface Seg {
  pct: number;
  tone: "blue" | "cyan" | "amber" | "green" | "inert";
}

/// Stacked segments in one track — host memory shows allocated (blue) plus
/// migrating-in (cyan) over the free remainder.
export function Bar({
  segments,
  width,
  size,
}: {
  segments: Seg[];
  width?: number | string;
  size?: "thick" | "fat";
}) {
  return (
    <div
      className={`vq-bar${size ? ` ${size}` : ""}`}
      style={width != null ? { width, flex: "0 0 auto" } : undefined}
    >
      {segments.map((s, i) => (
        <span
          key={i}
          className={`vq-bar-${s.tone}`}
          style={{ width: `${Math.max(0, Math.min(100, s.pct))}%` }}
        />
      ))}
    </div>
  );
}

/// A progress bar with its label to the right, in the fill's colour.
export function ProgressCell({
  pct,
  label,
  tone = "cyan",
  width = 64,
}: {
  pct: number;
  label: ReactNode;
  tone?: "blue" | "cyan" | "amber";
  width?: number;
}) {
  return (
    <div className="vq-barcell">
      <Bar segments={[{ pct, tone }]} width={width} />
      <span className="vq-mono-sm" style={{ color: `var(--vq-${tone})` }}>
        {label}
      </span>
    </div>
  );
}

/* ----------------------------------------------------------- metric card -- */

export function Metric({
  label,
  value,
  unit,
  tone,
  bar,
  caption,
  captionTone,
}: {
  label: string;
  value: ReactNode;
  unit?: ReactNode;
  tone?: "cyan" | "amber" | "red" | "green" | "blue";
  bar?: Seg[];
  caption?: ReactNode;
  captionTone?: "cyan" | "amber" | "red";
}) {
  return (
    <div className="vq-metric">
      <div className="vq-metric-label">{label}</div>
      <div className="vq-metric-value" style={tone ? { color: `var(--vq-${tone})` } : undefined}>
        <span>{value}</span>
        {unit && <span className="vq-metric-unit">{unit}</span>}
      </div>
      {bar && <Bar segments={bar} />}
      {caption && (
        <div className="vq-metric-cap" style={captionTone ? { color: `var(--vq-${captionTone})` } : undefined}>
          {caption}
        </div>
      )}
    </div>
  );
}

/* --------------------------------------------------------------- buttons -- */

type BtnKind = "primary" | "secondary" | "destructive" | "caution" | "live" | "link";

export function Btn({
  kind = "secondary",
  tall,
  children,
  ...rest
}: {
  kind?: BtnKind;
  tall?: boolean;
  children: ReactNode;
} & React.ButtonHTMLAttributes<HTMLButtonElement>) {
  const cls = kind === "secondary" ? "" : ` ${kind}`;
  return (
    <button {...rest} className={`vq-btn${cls}${tall ? " tall" : ""}`}>
      {children}
    </button>
  );
}

/* -------------------------------------------------------------- controls -- */

export type FieldState = "default" | "overridden" | "inherited" | "invalid";

export function Field({
  label,
  help,
  state = "default",
  children,
}: {
  label?: string;
  help?: ReactNode;
  state?: FieldState;
  children: ReactNode;
}) {
  return (
    <div className="vq-field">
      {label && <div className="vq-label">{label}</div>}
      {children}
      {help && (
        <div
          className={`vq-help${state === "invalid" ? " err" : ""}${
            state === "overridden" ? " override" : ""
          }`}
        >
          {help}
        </div>
      )}
    </div>
  );
}

export function Input({
  state = "default",
  ...rest
}: { state?: FieldState } & InputHTMLAttributes<HTMLInputElement>) {
  return <input {...rest} className={`vq-input${state === "default" ? "" : ` ${state}`}`} />;
}

export function Select({
  state = "default",
  children,
  ...rest
}: { state?: FieldState; children: ReactNode } & SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <div className="vq-selectwrap">
      <select {...rest} className={`vq-select${state === "default" ? "" : ` ${state}`}`}>
        {children}
      </select>
    </div>
  );
}

export function Segmented<T extends string>({
  value,
  options,
  onChange,
  size,
  grow,
  mono,
}: {
  value: T;
  options: { value: T; label: ReactNode; tone?: "cyan" }[];
  onChange: (v: T) => void;
  size?: "tall" | "mini";
  grow?: boolean;
  mono?: boolean;
}) {
  return (
    <div className={`vq-seg${size ? ` ${size}` : ""}${grow ? " grow" : ""}${mono ? " mono" : ""}`}>
      {options.map((o) => (
        <button
          key={o.value}
          className={o.value === value ? "on" : ""}
          style={o.tone && o.value !== value ? { color: `var(--vq-${o.tone})` } : undefined}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function Toggle({
  on,
  onChange,
  label,
  disabled,
}: {
  on: boolean;
  onChange: (v: boolean) => void;
  label?: ReactNode;
  disabled?: boolean;
}) {
  const btn = (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      disabled={disabled}
      className={`vq-toggle${on ? " on" : ""}`}
      onClick={() => onChange(!on)}
    >
      <span />
    </button>
  );
  if (!label) return btn;
  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 10 }}>
      {btn}
      <span style={{ fontSize: 12.5 }}>{label}</span>
    </span>
  );
}

export function Check({
  on,
  onChange,
  label,
}: {
  on: boolean;
  onChange: (v: boolean) => void;
  label?: ReactNode;
}) {
  const box = (
    <button
      type="button"
      role="checkbox"
      aria-checked={on}
      className={`vq-check${on ? " on" : ""}`}
      onClick={() => onChange(!on)}
    >
      ✓
    </button>
  );
  if (!label) return box;
  return (
    <label className="vq-checkrow">
      {box}
      {label}
    </label>
  );
}

/* --------------------------------------------------- empty / error / load -- */

export function EmptyState({ headline, hint }: { headline: string; hint?: ReactNode }) {
  return (
    <div className="vq-empty">
      <MarkWatermark size={34} />
      <div className="headline">{headline}</div>
      {hint && <div className="hint">{hint}</div>}
    </div>
  );
}

/// Always shows the raw message — operators need it.
export function ErrorPanel({ summary, detail }: { summary: string; detail?: unknown }) {
  const raw = detail instanceof Error ? detail.message : detail ? String(detail) : null;
  return (
    <div className="vq-errorpanel">
      <div className="summary">{summary}</div>
      {raw && <div className="raw">{raw}</div>}
    </div>
  );
}

/// Renders any query error, or nothing.
export function QueryError({ error, what }: { error: unknown; what: string }) {
  if (!error) return null;
  return <ErrorPanel summary={`Could not load ${what}`} detail={error} />;
}

/// Skeleton rows, never a spinner. Staggered widths so the shape reads as
/// content rather than a progress bar.
export function SkeletonRows({ cols, rows = 5 }: { cols: string; rows?: number }) {
  const widths = ["70%", "100%", "88%", "54%"];
  const n = cols.trim().split(/\s+/).length;
  return (
    <>
      {Array.from({ length: rows }, (_, r) => (
        <div key={r} className="vq-tr" style={{ gridTemplateColumns: cols }}>
          {Array.from({ length: n }, (_, c) => (
            <div key={c} className="vq-skel" style={{ width: widths[(r + c) % widths.length] }} />
          ))}
        </div>
      ))}
    </>
  );
}

/* ------------------------------------------------------------ dialog bits -- */

export function DialogHead({ children }: { children: ReactNode }) {
  return <div className="vq-dialog-title">{children}</div>;
}
export function DialogBody({ children }: { children: ReactNode }) {
  return <div className="vq-dialog-body">{children}</div>;
}
export function DialogFoot({ children }: { children: ReactNode }) {
  return <div className="vq-dialog-foot">{children}</div>;
}

/* --------------------------------------------------------- key/value rows -- */

export function KV({
  k,
  v,
  labelWidth = 150,
}: {
  k: ReactNode;
  v: ReactNode;
  labelWidth?: number;
}) {
  return (
    <div className="vq-kv" style={{ gridTemplateColumns: `${labelWidth}px 1fr` }}>
      <div className="k">{k}</div>
      <div className="v">{v}</div>
    </div>
  );
}
