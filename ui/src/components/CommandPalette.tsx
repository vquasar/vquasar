// The command palette (⌘K / Ctrl-K).
//
// Two decisions shape this, and both are about not lying to the operator.
//
// It searches **only what the console has already loaded**. Every list here
// comes from a query hook that is already permission-gated and already cached,
// so the palette issues no requests of its own: it cannot be slow, and it
// cannot show a caller a resource they are not allowed to read — the hook
// returns nothing for them, so there is nothing to filter out afterwards.
// Filtering after the fact is how a palette becomes an enumeration oracle.
//
// And the matching is plain substring, ranked by where the match falls. Not
// fuzzy. A fuzzy matcher reorders results in ways nobody can predict, and the
// whole value of a palette is that typing the same three letters gets you to
// the same place every time.

import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import Dialog from "@mui/material/Dialog";
import {
  useHosts,
  useImages,
  useNetworks,
  useStoragePools,
  useTemplates,
  useVms,
  useVolumes,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { KindIcon } from "../ui/icons";
import { ACTION, READ } from "../auth/perm";
import type { Permission } from "../auth/perm";

interface Entry {
  /// What the operator sees and types against.
  label: string;
  /// The group heading, and the thing that disambiguates two same-named rows.
  kind: string;
  to: string;
  /// Extra text that matches but is shown quietly — an id, a host name.
  hint?: string;
}

/// Where a match falls in the label, or `null` for no match.
///
/// Rank is the match position: a name that *starts* with what you typed beats
/// one that merely contains it, and among equals the shorter label wins. That
/// is the whole ranking, and it is worth keeping that small — a palette people
/// can predict is one they use without looking.
function rank(entry: Entry, q: string): number | null {
  const label = entry.label.toLowerCase();
  const at = label.indexOf(q);
  if (at >= 0) return at * 1000 + label.length;
  // A hint match still counts, but never outranks a label match.
  if (entry.hint?.toLowerCase().includes(q)) return 1_000_000;
  return null;
}

/// The pages a caller can actually open, and the things they can create.
function staticEntries(can: (p: Permission) => boolean): Entry[] {
  const pages: [string, string, Permission | null][] = [
    ["Overview", "/", null],
    ["Hosts", "/hosts", READ.hosts],
    ["Virtual machines", "/vms", READ.vms],
    ["Images", "/images", READ.images],
    ["Templates", "/templates", READ.templates],
    ["Networks", "/networks", READ.networks],
    ["Security groups", "/security-groups", READ.securityGroups],
    ["Volumes", "/volumes", READ.volumes],
    ["Storage pools", "/storage-pools", READ.storagePools],
    ["Tasks", "/tasks", READ.tasks],
    ["Events", "/events", READ.events],
    ["Projects", "/projects", READ.projects],
    ["Access control", "/iam", ACTION.iamRead],
    ["Settings", "/settings", null],
  ];
  const out: Entry[] = pages
    .filter(([, , perm]) => perm === null || can(perm))
    .map(([label, to]) => ({ label, kind: "Page", to }));
  // Actions, not pages: listed separately so "create" finds them, and gated on
  // the permission the handler actually requires rather than on the page.
  if (can(ACTION.vmCreate)) {
    out.push({ label: "Create a virtual machine", kind: "Action", to: "/vms/new" });
  }
  return out;
}

function useEntries(): Entry[] {
  const { can } = usePermissions();
  const vms = useVms();
  const hosts = useHosts();
  const networks = useNetworks();
  const volumes = useVolumes();
  const pools = useStoragePools();
  const images = useImages();
  const templates = useTemplates();

  return useMemo(() => {
    const out = staticEntries(can);
    for (const v of vms.data ?? []) {
      out.push({ label: v.name, kind: "VM", to: `/vms/${v.id}`, hint: v.id });
    }
    for (const h of hosts.data ?? []) {
      out.push({ label: h.name, kind: "Host", to: `/hosts/${h.id}`, hint: h.id });
    }
    // The rest have no detail route, so they land on their list. Still worth
    // being here: "where is that volume" is answered by the page it is on.
    for (const n of networks.data ?? []) {
      out.push({ label: n.name, kind: "Network", to: "/networks", hint: n.id });
    }
    for (const v of volumes.data ?? []) {
      out.push({ label: v.name, kind: "Volume", to: "/volumes", hint: v.id });
    }
    for (const p of pools.data ?? []) {
      out.push({ label: p.name, kind: "Storage pool", to: "/storage-pools", hint: p.id });
    }
    for (const i of images.data ?? []) {
      out.push({ label: i.name, kind: "Image", to: "/images", hint: i.id });
    }
    for (const t of templates.data ?? []) {
      out.push({ label: t.name, kind: "Template", to: `/templates/${t.id}/launch`, hint: t.id });
    }
    return out;
  }, [can, vms.data, hosts.data, networks.data, volumes.data, pools.data, images.data, templates.data]);
}

const LIMIT = 12;

export function CommandPalette({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [q, setQ] = useState("");
  const [cursor, setCursor] = useState(0);
  const navigate = useNavigate();
  const entries = useEntries();
  const listRef = useRef<HTMLDivElement>(null);
  // The shortcut listener is registered once; this keeps it calling the
  // current setter without re-binding on every render.
  const setOpenRef = useRef(onOpenChange);
  setOpenRef.current = onOpenChange;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        // Preventing the default matters: Ctrl-K is the browser's search bar
        // on some builds, and a palette that fights the address bar is worse
        // than no palette.
        e.preventDefault();
        setQ("");
        setCursor(0);
        setOpenRef.current(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const results = useMemo(() => {
    const needle = q.trim().toLowerCase();
    // Empty query shows the pages, not a firehose of every VM in the fleet.
    if (!needle) return entries.filter((e) => e.kind === "Page" || e.kind === "Action");
    return entries
      .map((e) => ({ e, r: rank(e, needle) }))
      .filter((x): x is { e: Entry; r: number } => x.r !== null)
      .sort((a, b) => a.r - b.r)
      .slice(0, LIMIT)
      .map((x) => x.e);
  }, [entries, q]);

  // Keep the cursor inside the list as it shrinks under a longer query.
  useEffect(() => setCursor(0), [q]);

  const go = (e: Entry | undefined) => {
    if (!e) return;
    onOpenChange(false);
    navigate(e.to);
  };

  return (
    <Dialog
      open={open}
      onClose={() => onOpenChange(false)}
      maxWidth="sm"
      fullWidth
      // Anchored high: a palette that opens in the middle of the screen moves
      // the reader's eye away from where they were working.
      sx={{ "& .MuiDialog-container": { alignItems: "flex-start", pt: "12vh" } }}
    >
      <div className="vq-palette" role="dialog" aria-label="Command palette">
        <input
          className="vq-palette-input"
          autoFocus
          value={q}
          placeholder="Jump to a VM, host, network, volume, page…"
          aria-label="Search"
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setCursor((c) => Math.min(c + 1, results.length - 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setCursor((c) => Math.max(c - 1, 0));
            } else if (e.key === "Enter") {
              e.preventDefault();
              go(results[cursor]);
            } else if (e.key === "Escape") {
              onOpenChange(false);
            }
          }}
        />
        <div className="vq-palette-list" ref={listRef}>
          {results.length === 0 && (
            <div className="vq-palette-empty">
              Nothing matches. The palette searches what this console has loaded — a resource you
              cannot read is not here.
            </div>
          )}
          {results.map((e, i) => (
            <button
              key={`${e.kind}:${e.to}:${e.label}`}
              className={`vq-palette-row${i === cursor ? " on" : ""}`}
              onMouseEnter={() => setCursor(i)}
              onClick={() => go(e)}
            >
              {/* The glyph carries the kind, so the word can stay quiet at
                  the end of the row rather than competing with the name. */}
              <KindIcon kind={e.kind} size={14} />
              <span className="vq-palette-label">{e.label}</span>
              <span className="vq-palette-kind">{e.kind}</span>
            </button>
          ))}
        </div>
      </div>
    </Dialog>
  );
}
