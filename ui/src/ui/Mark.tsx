// The vQuasar mark: an accretion plane, two opposing jets, and a core whose
// centre is a punched hole (an SVG mask, not a white dot) so the mark sits on
// any surface unedited. Three geometry builds — the primary mark is never
// scaled below 32px; smaller sizes shorten the tail, thicken the plane and
// merge the jets into the core (brand guidelines, "Size builds").

import { useId } from "react";

export function Mark({ size = 22, title }: { size?: number; title?: string }) {
  const maskId = useId();
  const build = size >= 32 ? "full" : size >= 20 ? "md" : "sm";

  return (
    <svg
      viewBox="0 0 64 64"
      width={size}
      height={size}
      role={title ? "img" : "presentation"}
      aria-label={title}
      aria-hidden={title ? undefined : true}
      style={{ flex: `0 0 ${size}px` }}
    >
      {title && <title>{title}</title>}
      <mask id={maskId}>
        <rect width="64" height="64" fill="#fff" />
        <circle cx="32" cy="32" r={build === "full" ? 3.2 : 3} fill="#000" />
      </mask>
      <g mask={`url(#${maskId})`} fill="currentColor">
        <g transform="rotate(-14 32 32)">
          <path
            d="M52 25.5A26 9 0 1 0 52 38.5"
            fill="none"
            stroke="currentColor"
            strokeWidth={build === "full" ? 4.4 : build === "md" ? 5 : 6}
            strokeLinecap="round"
          />
          {build === "full" && (
            <path
              d="M52 38.5 60 42"
              fill="none"
              stroke="currentColor"
              strokeWidth={4.4}
              strokeLinecap="round"
            />
          )}
        </g>
        {build === "full" && (
          <>
            <polygon points="28.5,34 35.5,32.5 43,5" />
            <polygon points="35.5,30 28.5,31.5 21,59" />
            <circle cx="32" cy="32" r="7.5" />
          </>
        )}
        {build === "md" && (
          <>
            <polygon points="28,34.5 36.5,32 43,5" />
            <polygon points="36.5,29.5 28,31.5 21,59" />
            <circle cx="32" cy="32" r="8" />
          </>
        )}
        {build === "sm" && (
          <>
            <polygon points="27,35 37,32 44,4" />
            <polygon points="37,29 27,32 20,60" />
            <circle cx="32" cy="32" r="9" />
          </>
        )}
      </g>
    </svg>
  );
}

/// Outline build. Behind diagrams and in empty states — a line colour, never a
/// logo.
export function MarkWatermark({ size = 34 }: { size?: number }) {
  return (
    <svg viewBox="0 0 64 64" width={size} height={size} aria-hidden="true">
      <g fill="none" stroke="currentColor" strokeWidth="1.6">
        <g transform="rotate(-14 32 32)">
          <path d="M52 25.5A26 9 0 1 0 52 38.5" />
          <path d="M52 38.5 61 43" />
        </g>
        <polygon points="25,36 37,31.5 45,4" />
        <polygon points="39,28 27,32.5 19,60" />
        <circle cx="32" cy="32" r="7.5" />
      </g>
    </svg>
  );
}

/// The application header lockup: mark + live-text wordmark. The lowercase `v`
/// is smaller and lighter but joined — never `v Quasar`.
export function Logo({ size = 22 }: { size?: number }) {
  return (
    <>
      <Mark size={size} title="vQuasar" />
      <span className="vq-wordmark">
        <i>v</i>
        <b>Quasar</b>
      </span>
    </>
  );
}
