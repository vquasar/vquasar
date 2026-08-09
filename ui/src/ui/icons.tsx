// The icon family.
//
// One file, one grid, one stroke weight. An icon set drifts the moment two
// glyphs are drawn at different weights or on different grids — it stops
// reading as a family and starts reading as clip art — so every icon here is
// a 16×16 viewBox, 1.5-unit stroke, round caps and joins, and nothing else.
//
// They are **functional**, not decorative. Each one stands for a resource kind
// the operator already thinks in — a host, a VM, a network, a volume — and is
// used where shape helps you find a thing faster than reading does: the nav,
// and the palette's kind column. A heading with an icon beside its own word is
// noise, and there are none.
//
// Every glyph draws in `currentColor` and is `aria-hidden`. In every place
// these are used the label is right there in the DOM, so announcing the icon
// too would read the same thing twice. An icon that is ever the *only* content
// of a control needs its own label at the call site — there are none today,
// and this comment is the reason someone should hesitate before adding one.
//
// No icon package. The console already ships MUI; pulling a second library for
// fourteen glyphs is bundle weight for nothing, and a set we draw is a set we
// can keep coherent.

import type { ReactNode } from "react";

export interface IconProps {
  size?: number;
  className?: string;
}

function Svg({ size = 15, className, children }: IconProps & { children: ReactNode }) {
  return (
    <svg
      viewBox="0 0 16 16"
      width={size}
      height={size}
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      // Nailed down so a flex parent cannot squash a glyph into an ellipse.
      style={{ flex: `0 0 ${size}px` }}
      className={className}
    >
      {children}
    </svg>
  );
}

/* ------------------------------------------------------------- resources -- */

/// A host: the machine itself, seen from the front.
export const HostIcon = (p: IconProps) => (
  <Svg {...p}>
    <rect x="2" y="3" width="12" height="4.5" rx="1" />
    <rect x="2" y="8.5" width="12" height="4.5" rx="1" />
    <path d="M4.5 5.25h.01M4.5 10.75h.01" />
  </Svg>
);

/// A VM: a screen, because that is what a guest is to the person using it.
export const VmIcon = (p: IconProps) => (
  <Svg {...p}>
    <rect x="2" y="3" width="12" height="8" rx="1.5" />
    <path d="M6 13.5h4" />
  </Svg>
);

/// A network: nodes joined, not a globe — this is an L2 segment, not the
/// internet.
export const NetworkIcon = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="8" cy="3.5" r="1.6" />
    <circle cx="3.5" cy="12.5" r="1.6" />
    <circle cx="12.5" cy="12.5" r="1.6" />
    <path d="M8 5.1v3.4M8 8.5 4.4 11.2M8 8.5l3.6 2.7" />
  </Svg>
);

/// A security group: a shield, the one place a lock-like shape is honest.
export const ShieldIcon = (p: IconProps) => (
  <Svg {...p}>
    <path d="M8 2 13 4v4c0 3-2.2 5.2-5 6-2.8-.8-5-3-5-6V4Z" />
  </Svg>
);

/// A volume: a disk seen edge-on.
export const VolumeIcon = (p: IconProps) => (
  <Svg {...p}>
    <ellipse cx="8" cy="4" rx="5.5" ry="2" />
    <path d="M2.5 4v8c0 1.1 2.5 2 5.5 2s5.5-.9 5.5-2V4" />
    <path d="M2.5 8c0 1.1 2.5 2 5.5 2s5.5-.9 5.5-2" />
  </Svg>
);

/// A storage pool: the same disk, stacked — a pool is where volumes go.
export const PoolIcon = (p: IconProps) => (
  <Svg {...p}>
    <ellipse cx="8" cy="3.5" rx="5.5" ry="1.8" />
    <path d="M2.5 3.5v3c0 1 2.5 1.8 5.5 1.8s5.5-.8 5.5-1.8v-3" />
    <path d="M2.5 9.5c0 1 2.5 1.8 5.5 1.8s5.5-.8 5.5-1.8" />
    <path d="M2.5 6.5v6c0 1 2.5 1.8 5.5 1.8s5.5-.8 5.5-1.8v-6" />
  </Svg>
);

/// An image: a golden disk, drawn as a layered plate to separate it from a
/// volume at a glance.
export const ImageIcon = (p: IconProps) => (
  <Svg {...p}>
    <path d="M8 1.8 14 5 8 8.2 2 5Z" />
    <path d="M2 8.4 8 11.6l6-3.2M2 11.4 8 14.6l6-3.2" />
  </Svg>
);

/// A template: a stencil — an outline something is cut from.
export const TemplateIcon = (p: IconProps) => (
  <Svg {...p}>
    <rect x="2.5" y="2.5" width="11" height="11" rx="1.5" strokeDasharray="2.6 2" />
    <path d="M6 8h4M8 6v4" />
  </Svg>
);

/* --------------------------------------------------------------- process -- */

/// A task: work in flight.
export const TaskIcon = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="8" cy="8" r="5.8" />
    <path d="M8 4.6V8l2.4 1.6" />
  </Svg>
);

/// An event: something that happened, and was written down.
export const EventIcon = (p: IconProps) => (
  <Svg {...p}>
    <path d="M3 3.5h10M3 8h10M3 12.5h6" />
  </Svg>
);

/* ---------------------------------------------------------------- people -- */

/// A project: the tenancy boundary, drawn as the folder it behaves like.
export const ProjectIcon = (p: IconProps) => (
  <Svg {...p}>
    <path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h2.6l1.4 1.8h5A1.5 1.5 0 0 1 14 6.3v5.2A1.5 1.5 0 0 1 12.5 13h-9A1.5 1.5 0 0 1 2 11.5Z" />
  </Svg>
);

/// Access control: a person, because roles are held by people.
export const IamIcon = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="8" cy="5.5" r="2.5" />
    <path d="M3 13.2c.9-2.3 2.7-3.4 5-3.4s4.1 1.1 5 3.4" />
  </Svg>
);

/* ------------------------------------------------------------------ misc -- */

/// The fleet overview.
export const OverviewIcon = (p: IconProps) => (
  <Svg {...p}>
    <rect x="2.5" y="2.5" width="5" height="5" rx="1" />
    <rect x="8.5" y="2.5" width="5" height="5" rx="1" />
    <rect x="2.5" y="8.5" width="5" height="5" rx="1" />
    <rect x="8.5" y="8.5" width="5" height="5" rx="1" />
  </Svg>
);

/// Settings.
export const SettingsIcon = (p: IconProps) => (
  <Svg {...p}>
    <path d="M2.5 4.5h11M2.5 11.5h11" />
    <circle cx="6" cy="4.5" r="1.8" />
    <circle cx="10" cy="11.5" r="1.8" />
  </Svg>
);

/// A thing the operator does, rather than a thing they look at. Used in the
/// palette for entries that create something.
export const ActionIcon = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="8" cy="8" r="5.8" />
    <path d="M8 5.4v5.2M5.4 8h5.2" />
  </Svg>
);

/* ------------------------------------------------------------------- map -- */

/// The one place a resource kind is turned into a glyph.
///
/// A map rather than a `switch` at each call site: two call sites choosing
/// their own icon for "Volume" is how a family stops being one.
export const KIND_ICONS: Record<string, (p: IconProps) => ReactNode> = {
  Overview: OverviewIcon,
  Host: HostIcon,
  VM: VmIcon,
  Network: NetworkIcon,
  "Security group": ShieldIcon,
  Volume: VolumeIcon,
  "Storage pool": PoolIcon,
  Image: ImageIcon,
  Template: TemplateIcon,
  Task: TaskIcon,
  Event: EventIcon,
  Project: ProjectIcon,
  "Access control": IamIcon,
  Settings: SettingsIcon,
  Action: ActionIcon,
  Page: OverviewIcon,
};

/// The icon for a kind, or `null` when there is none.
///
/// Null rather than a fallback glyph: a wrong-but-present icon is read as
/// meaning something, and a missing one is read as nothing, which is the
/// truth when a kind has no glyph yet.
export function KindIcon({ kind, size }: { kind: string; size?: number }) {
  const Icon = KIND_ICONS[kind];
  return Icon ? <>{Icon({ size })}</> : null;
}
