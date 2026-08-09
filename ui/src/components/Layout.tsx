// The app shell: a 224px grouped sidebar with live counts, a 48px top bar, and
// the content column (handoff, "App shell").
//
// The navigation is text-only on purpose. The functional icon family has not
// been designed yet, and the brand guidelines are explicit that a generic icon
// set must not stand in for it.

import { useState, type ReactNode } from "react";
import { Link, useLocation } from "react-router-dom";
import Menu from "@mui/material/Menu";
import MenuItem from "@mui/material/MenuItem";
import {
  useHosts,
  useImages,
  useNetworks,
  useSecurityGroups,
  useTasks,
  useTemplates,
  useVms,
  useControlConfig,
  useStoragePools,
  useVolumes,
} from "../api/hooks";
import { useAuth } from "../auth/AuthProvider";
import { ProjectSwitch } from "./ProjectSwitch";
import { CommandPalette } from "./CommandPalette";
import { useProject } from "../auth/ProjectProvider";
import { formatRelative } from "../format";
import { useProjects } from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { ACTION, READ } from "../auth/perm";
import { useThemeMode } from "../theme/ThemeMode";
import { Logo } from "../ui/Mark";
import { Segmented } from "../ui/kit";
import { initials, relTime } from "../format";
import { CrumbProvider, useCrumbLabel } from "./Breadcrumb";

interface NavItem {
  to: string;
  label: string;
  count?: number;
}

function NavGroup({ label, items }: { label: string; items: NavItem[] }) {
  const { pathname } = useLocation();
  // A detail route keeps its parent active: /vms/:id highlights Virtual machines.
  const isActive = (to: string) =>
    to === "/" ? pathname === "/" : pathname === to || pathname.startsWith(`${to}/`);

  return (
    <div className="vq-navgroup">
      <div className="vq-navlabel">{label}</div>
      {items.map((it) => (
        <Link key={it.to} to={it.to} className={`vq-navitem${isActive(it.to) ? " active" : ""}`}>
          <span>{it.label}</span>
          <span className="vq-navcount">{it.count != null ? it.count : "—"}</span>
        </Link>
      ))}
    </div>
  );
}

function AgentStatus() {
  const hosts = useHosts();
  const list = hosts.data ?? [];
  const connected = list.filter((h) => h.state === "Ready").length;
  // The freshest heartbeat is the closest thing the API gives us to "when did
  // the fleet last check in".
  const newest = list
    .map((h) => h.last_heartbeat)
    .filter((t): t is string => !!t)
    .sort()
    .at(-1);

  return (
    <div className="vq-sidefoot">
      <div className="vq-sidefoot-agents">
        <span
          className="vq-pulse"
          style={{
            width: 6,
            height: 6,
            borderRadius: "50%",
            background: connected === list.length && list.length > 0 ? "var(--vq-cyan)" : "var(--vq-amber)",
          }}
        />
        {connected} / {list.length} agents connected
      </div>
      <div className="vq-sidefoot-meta">
        {newest ? `last heartbeat ${relTime(newest)}` : "awaiting first heartbeat"}
      </div>
    </div>
  );
}

function Sidebar() {
  const { can } = usePermissions();
  const hosts = useHosts();
  const vms = useVms();
  const images = useImages();
  const templates = useTemplates();
  const networks = useNetworks();
  const sgs = useSecurityGroups();
  const volumes = useVolumes();
  const pools = useStoragePools();
  const tasks = useTasks();
  const { enabled: tenancy } = useProject();
  const projects = useProjects(tenancy);

  const openTasks = (tasks.data ?? []).filter(
    (t) => t.state === "Pending" || t.state === "Running",
  ).length;

  const operations: NavItem[] = [
    { to: "/tasks", label: "Tasks", count: tasks.data ? openTasks : undefined },
    { to: "/events", label: "Events" },
    // Only when tenancy is on: a projects page in a single-tenant install is a
    // catalogue of one, and the top bar hides the switcher for the same reason.
    ...(tenancy && can(READ.projects)
      ? [{ to: "/projects", label: "Projects", count: projects.data?.length }]
      : []),
    ...(can(ACTION.iamRead) ? [{ to: "/iam", label: "Access control" }] : []),
    { to: "/settings", label: "Settings" },
  ];

  return (
    <nav className="vq-sidebar">
      <Link to="/" className="vq-brand">
        <Logo size={22} />
      </Link>
      <div className="vq-nav">
        <NavGroup
          label="Fleet"
          items={[
            { to: "/", label: "Overview" },
            { to: "/hosts", label: "Hosts", count: hosts.data?.length },
          ]}
        />
        <NavGroup
          label="Compute"
          items={[
            { to: "/vms", label: "Virtual machines", count: vms.data?.length },
            { to: "/images", label: "Images", count: images.data?.length },
            { to: "/templates", label: "Templates", count: templates.data?.length },
          ]}
        />
        <NavGroup
          label="Network & storage"
          items={[
            { to: "/networks", label: "Networks", count: networks.data?.length },
            { to: "/security-groups", label: "Security groups", count: sgs.data?.length },
            { to: "/volumes", label: "Volumes", count: volumes.data?.length },
            ...(can(READ.storagePools)
              ? [{ to: "/storage-pools", label: "Storage pools", count: pools.data?.length }]
              : []),
          ]}
        />
        <NavGroup label="Operations" items={operations} />
      </div>
      {can(READ.hosts) && <AgentStatus />}
      {can(READ.hosts) && <ControlPlaneFoot />}
    </nav>
  );
}

/// Which control plane this console is talking to, and whether its reconcile
/// loop is still turning.
///
/// The second half is the point. A stopped loop looks exactly like a fleet with
/// nothing to do — every VM simply stays as it was — so the age of the last
/// completed pass is the only thing that tells them apart, and it belongs
/// somewhere an operator sees without going looking.
function ControlPlaneFoot() {
  const cfg = useControlConfig();
  if (!cfg.data) return null;
  const { version, reconcile } = cfg.data;
  const last = reconcile.last_pass_at ? Date.parse(reconcile.last_pass_at) : null;
  // Stale at four intervals: one missed tick is a slow pass, four is a loop
  // that has stopped.
  const stale =
    last === null || Date.now() - last > reconcile.interval_secs * 4000;
  return (
    <div className="vq-cpfoot" title="Control-plane version, and when its reconcile loop last completed a pass">
      <span className="vq-mono">v{version}</span>
      <span className={stale ? "t-amber" : "t-3"}>
        {reconcile.last_pass_at
          ? `reconciled ${formatRelative(reconcile.last_pass_at)}`
          : "never reconciled"}
      </span>
    </div>
  );
}

const CRUMB: Record<string, string> = {
  "": "Overview",
  hosts: "Hosts",
  vms: "Virtual machines",
  images: "Images",
  templates: "Templates",
  networks: "Networks",
  "security-groups": "Security groups",
  volumes: "Volumes",
  "storage-pools": "Storage pools",
  tasks: "Tasks",
  events: "Events",
  iam: "Access control",
  settings: "Settings",
};

function Breadcrumb() {
  const { pathname } = useLocation();
  const published = useCrumbLabel();
  const segs = pathname.split("/").filter(Boolean);
  // A detail route names its resource; the collection name is the fallback.
  const current = published ?? CRUMB[segs[0] ?? ""] ?? segs[0];
  return (
    <div className="vq-crumb">
      <span>{window.location.hostname}</span>
      <span className="sep">/</span>
      <span className="cur">{current}</span>
    </div>
  );
}

function ThemeSwitch() {
  const { mode, setMode } = useThemeMode();
  return (
    <Segmented
      value={mode}
      size="mini"
      mono
      onChange={setMode}
      options={[
        { value: "dark", label: "DARK" },
        { value: "light", label: "LIGHT" },
      ]}
    />
  );
}

function UserMenu() {
  const { enabled, profile, logout } = useAuth();
  const [anchor, setAnchor] = useState<null | HTMLElement>(null);
  const name =
    (profile?.preferred_username as string) ||
    (profile?.name as string) ||
    (profile?.email as string) ||
    (enabled ? "account" : "dev");

  return (
    <>
      <button className="vq-user" onClick={(e) => setAnchor(e.currentTarget)}>
        <span className="vq-avatar">{initials(name)}</span>
        {name}
      </button>
      <Menu anchorEl={anchor} open={!!anchor} onClose={() => setAnchor(null)}>
        {enabled ? (
          <MenuItem
            onClick={() => {
              setAnchor(null);
              logout();
            }}
          >
            Sign out
          </MenuItem>
        ) : (
          <MenuItem disabled>Authentication disabled</MenuItem>
        )}
      </Menu>
    </>
  );
}

export function Layout({ children }: { children: ReactNode }) {
  const { pathname } = useLocation();
  const [palette, setPalette] = useState(false);
  return (
    <CrumbProvider>
      <div className="vq-app">
        <Sidebar />
        <div className="vq-body">
          <header className="vq-topbar">
            <Breadcrumb />
            <div className="vq-spacer" />
            {/* The affordance and the palette are the same thing now. */}
            <button className="vq-cmdk" onClick={() => setPalette(true)}>
              <kbd>⌘K</kbd>
              Search or run a command
            </button>
            <ProjectSwitch />
            <ThemeSwitch />
            <UserMenu />
          </header>
          <main className={`vq-main${pathname === "/" ? " wide-gap" : ""}`}>{children}</main>
        </div>
      </div>
      <CommandPalette open={palette} onOpenChange={setPalette} />
    </CrumbProvider>
  );
}
