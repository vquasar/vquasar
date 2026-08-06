// Networks (handoff §9). Isolation is the column that matters: a VLAN is local
// to a switch, a VXLAN overlay spans hosts — so the VNI is the one thing here
// rendered in cyan.

import { useMemo, useState } from "react";
import Dialog from "@mui/material/Dialog";
import {
  useCreateNetwork,
  useDeleteNetwork,
  useNetworkAllocations,
  useNetworks,
  useSecurityGroups,
  useUpdateNetwork,
  useVms,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { ACTION } from "../auth/perm";
import {
  Btn,
  Card,
  Dash,
  DialogBody,
  DialogFoot,
  DialogHead,
  EmptyState,
  ErrorPanel,
  Field,
  Grid,
  Input,
  PageHeader,
  QueryError,
  RowMenu,
  Select,
  SkeletonRows,
  Table,
  THead,
  TRow,
} from "../ui/kit";
import type {
  CreateNetworkRequest,
  Network,
  NetworkKind,
  SecurityGroupRule,
} from "../api/types";

const COLS = "1.2fr 110px 1.2fr 1fr 1.4fr 1fr 70px";
const RULE_COLS = "90px 90px 1fr 1fr";
const POOL_CELLS = 16;

const empty = (s: string) => (s.trim() === "" ? null : s.trim());

/// IPv4 dotted quad → integer, so a pool's size is arithmetic rather than a
/// guess. Returns null for anything that is not a plain v4 address.
function v4ToInt(ip: string | null): number | null {
  if (!ip) return null;
  const parts = ip.split(".");
  if (parts.length !== 4) return null;
  let n = 0;
  for (const p of parts) {
    const b = Number(p);
    if (!Number.isInteger(b) || b < 0 || b > 255) return null;
    n = n * 256 + b;
  }
  return n;
}

/// Whether a network's default policy lets anything in from anywhere. Every NIC
/// on the network inherits this group (ADR-017), so an unrestricted default is
/// a property of the network, not of one guest — worth saying out loud.
function allowsAllIngress(
  n: Network,
  groups: { id: string; rules: SecurityGroupRule[] }[],
): boolean {
  const g = groups.find((x) => x.id === n.default_security_group_id);
  if (!g) return false;
  return g.rules.some(
    (r) =>
      r.direction === "ingress" &&
      (r.remote_cidr === null || r.remote_cidr === "0.0.0.0/0" || r.remote_cidr === "::/0") &&
      (r.protocol === "any" || (r.port_min === null && r.port_max === null)),
  );
}

function poolSize(n: Network): number | null {
  const start = v4ToInt(n.pool_v4_start);
  const end = v4ToInt(n.pool_v4_end);
  if (start == null || end == null || end < start) return null;
  return end - start + 1;
}

/// The tail of an address, so a pool range reads ".2.10 – .3.250" rather than
/// two full addresses fighting for the column.
function tail(ip: string | null): string | null {
  if (!ip) return null;
  const parts = ip.split(".");
  return parts.length === 4 ? `.${parts[2]}.${parts[3]}` : ip;
}

function EditDialog({ edit, onClose }: { edit: Network | null; onClose: () => void }) {
  const create = useCreateNetwork();
  const update = useUpdateNetwork();
  const { can } = usePermissions();
  // Attaching to physical infrastructure is a platform decision: a VLAN tag is
  // a fact about the switch, and picking one picks which provider segment you
  // land on (ADR-016). An operator without it can still create tenant overlays.
  const platform = can(ACTION.networkCreateProvider);
  const [name, setName] = useState(edit?.name ?? "");
  const [kind, setKind] = useState<NetworkKind>(
    edit?.kind ?? (edit?.vni != null ? "tenant" : edit?.vlan != null ? "vlan" : platform ? "provider" : "tenant"),
  );
  const [uplink, setUplink] = useState(edit?.physical_network ?? "");
  const [vlan, setVlan] = useState(edit?.vlan != null ? String(edit.vlan) : "");
  const [cidr4, setCidr4] = useState(edit?.cidr_v4 ?? "");
  const [gw4, setGw4] = useState(edit?.gateway_v4 ?? "");
  const [cidr6, setCidr6] = useState(edit?.cidr_v6 ?? "");
  const [gw6, setGw6] = useState(edit?.gateway_v6 ?? "");
  const [dns, setDns] = useState((edit?.dns ?? []).join(", "));

  const submit = () => {
    const body: CreateNetworkRequest = {
      name,
      kind,
      // Only a physical network carries an uplink or a tag; sending either on a
      // tenant network is rejected.
      physical_network: kind === "tenant" ? null : empty(uplink),
      vlan: kind === "vlan" && vlan ? Number(vlan) : null,
      cidr_v4: empty(cidr4),
      gateway_v4: empty(gw4),
      cidr_v6: empty(cidr6),
      gateway_v6: empty(gw6),
      dns: dns
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean),
    };
    if (edit) update.mutate({ id: edit.id, body }, { onSuccess: onClose });
    else create.mutate(body, { onSuccess: onClose });
  };

  const busy = create.isPending || update.isPending;
  const err = create.error || update.error;

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>{edit ? "Edit network" : "Create network"}</DialogHead>
      <DialogBody>
        <Field label="Name">
          <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field
          label="Kind"
          help={
            kind === "tenant"
              ? "A VXLAN overlay with a platform-allocated VNI — the only kind that isolates by itself."
              : kind === "vlan"
                ? "802.1Q tag on the uplink. Isolated only as far as the switch honours the tag."
                : platform
                  ? "Untagged, attached to the uplink. Guarantees nothing by itself — its security is the physical network's."
                  : "Physical kinds need network:create:provider; you can create tenant overlays."
          }
        >
          <Select value={kind} onChange={(e) => setKind(e.target.value as NetworkKind)}>
            <option value="provider" disabled={!platform}>
              provider (untagged, physical)
            </option>
            <option value="vlan" disabled={!platform}>
              vlan (802.1Q, physical)
            </option>
            <option value="tenant">tenant (VXLAN overlay)</option>
          </Select>
        </Field>
        {kind !== "tenant" && (
          <Grid cols="1fr 1fr">
            <Field label="Uplink" help="Physical network to attach to. Blank means `default`.">
              <Input value={uplink} placeholder="default" onChange={(e) => setUplink(e.target.value)} />
            </Field>
            {kind === "vlan" && (
              <Field label="VLAN tag (1–4094)" help="Must be in the fleet's permitted range.">
                <Input value={vlan} onChange={(e) => setVlan(e.target.value)} />
              </Field>
            )}
          </Grid>
        )}
        {kind === "tenant" && edit?.vni != null && (
          <Field label="VNI" help="Allocated by the control plane; never caller-supplied.">
            <Input value={String(edit.vni)} disabled />
          </Field>
        )}
        <div className="vq-label" style={{ marginTop: 4 }}>
          IP management — leave a family blank for DHCP
        </div>
        <Grid cols="1fr 1fr">
          <Field label="IPv4 subnet">
            <Input
              value={cidr4}
              placeholder="192.168.222.0/24"
              onChange={(e) => setCidr4(e.target.value)}
            />
          </Field>
          <Field label="IPv4 gateway">
            <Input
              value={gw4}
              placeholder="192.168.222.1"
              onChange={(e) => setGw4(e.target.value)}
            />
          </Field>
        </Grid>
        <Grid cols="1fr 1fr">
          <Field label="IPv6 subnet">
            <Input value={cidr6} placeholder="fd00:56::/64" onChange={(e) => setCidr6(e.target.value)} />
          </Field>
          <Field label="IPv6 gateway">
            <Input value={gw6} placeholder="fd00:56::1" onChange={(e) => setGw6(e.target.value)} />
          </Field>
        </Grid>
        <Field label="DNS servers" help="Comma-separated.">
          <Input value={dns} placeholder="1.1.1.1, 8.8.8.8" onChange={(e) => setDns(e.target.value)} />
        </Field>
        {err && <ErrorPanel summary="Could not save the network" detail={err} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn kind="primary" onClick={submit} disabled={!name || busy}>
          {edit ? "Save" : "Create"}
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function IpAllocations({ network }: { network: Network }) {
  const allocs = useNetworkAllocations(network.id);
  const list = allocs.data ?? [];
  const size = poolSize(network);
  const used = list.length;
  const pct = size ? Math.min(100, (used / size) * 100) : 0;

  // Sixteen cells sample the pool; one cell per address would be a thousand
  // squares on a /22.
  const cells = Array.from({ length: POOL_CELLS }, (_, i) => {
    const allocated = size ? i < Math.round((used / size) * POOL_CELLS) : i < used;
    const reserved = allocated && list[i]?.vm_id == null;
    return reserved ? "reserved" : allocated ? "alloc" : "";
  });

  return (
    <Card title={`IP allocations · ${network.name}`} padded>
      <div style={{ display: "flex", justifyContent: "space-between", gap: 10 }}>
        <span className="vq-mono-sm">pool utilisation</span>
        <span className="vq-mono-sm t-2">
          {size ? `${used} / ${size} addresses` : `${used} assigned · no pool configured`}
        </span>
      </div>
      <div className="vq-bar fat" style={{ margin: "10px 0 14px" }}>
        <span className="vq-bar-blue" style={{ width: `${pct}%` }} />
      </div>
      <div className="vq-poolgrid">
        {cells.map((c, i) => (
          <div key={i} className={`vq-poolcell ${c}`} />
        ))}
      </div>
      <div className="vq-legend" style={{ marginTop: 12 }}>
        <span>
          <i style={{ background: "var(--vq-blue)" }} />
          allocated
        </span>
        <span>
          <i style={{ background: "var(--vq-cyan)" }} />
          reserved
        </span>
        <span>
          <i style={{ background: "var(--vq-surface-3)" }} />
          free
        </span>
      </div>
    </Card>
  );
}

export function Networks() {
  const networks = useNetworks();
  const vms = useVms();
  const sgs = useSecurityGroups();
  const del = useDeleteNetwork();
  const { can } = usePermissions();
  const [dialog, setDialog] = useState<{ edit: Network | null } | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  const list = networks.data ?? [];
  const vlanCount = list.filter((n) => n.vlan != null).length;
  const overlayCount = list.filter((n) => n.vni != null).length;

  // A network's NIC count comes from the VMs that reference it — no extra
  // request per row.
  const nicCounts = useMemo(() => {
    const m = new Map<string, number>();
    (vms.data ?? []).forEach((v) =>
      v.spec.network_interfaces.forEach((nic) =>
        m.set(nic.network_id, (m.get(nic.network_id) ?? 0) + 1),
      ),
    );
    return m;
  }, [vms.data]);

  const managed = list.filter((n) => n.cidr_v4);
  const focus = list.find((n) => n.id === selected) ?? managed[0] ?? list[0];
  const sg = sgs.data?.[0];
  const sgNicCount = useMemo(() => {
    if (!sg) return 0;
    return (vms.data ?? []).reduce(
      (n, v) =>
        n + v.spec.network_interfaces.filter((nic) => nic.security_groups?.includes(sg.id)).length,
      0,
    );
  }, [vms.data, sg]);

  return (
    <>
      <PageHeader
        title="Networks"
        subtitle={`${vlanCount} VLAN-backed · ${overlayCount} VXLAN overlay${
          overlayCount === 1 ? "" : "s"
        } · IPAM managed where a CIDR is set`}
        actions={
          (can(ACTION.networkCreate) || can(ACTION.networkCreateProvider)) && (
            <Btn kind="primary" onClick={() => setDialog({ edit: null })}>
              Create network
            </Btn>
          )
        }
      />

      <QueryError error={networks.error} what="networks" />
      {del.isError && <ErrorPanel summary="Delete failed" detail={del.error} />}

      <Table>
        <THead cols={COLS}>
          <div>Network</div>
          <div>Isolation</div>
          <div>CIDR v4</div>
          <div>Gateway</div>
          <div>Pool</div>
          <div>DNS</div>
          <div>NICs</div>
        </THead>

        {networks.isLoading && <SkeletonRows cols={COLS} />}

        {!networks.isLoading && list.length === 0 && (
          <div style={{ padding: 18 }}>
            <EmptyState
              headline="No networks yet"
              hint="Create one to give VMs somewhere to attach a NIC."
            />
          </div>
        )}

        {list.map((n) => {
          const pool =
            n.pool_v4_start && n.pool_v4_end
              ? `${tail(n.pool_v4_start)} – ${tail(n.pool_v4_end)}`
              : n.cidr_v4
                ? "whole subnet"
                : "external";
          const menu = [
            { label: "IP allocations", onClick: () => setSelected(n.id) },
            ...(can(ACTION.networkUpdate)
              ? [{ label: "Edit", onClick: () => setDialog({ edit: n }) }]
              : []),
            ...(can(ACTION.networkDelete)
              ? [{ label: "Delete", danger: true, onClick: () => del.mutate(n.id) }]
              : []),
          ];
          return (
            <TRow key={n.id} cols={COLS}>
              <div className="vq-cell" style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <button className="vq-btn link vq-name" onClick={() => setSelected(n.id)}>
                  {n.name}
                </button>
                {allowsAllIngress(n, sgs.data ?? []) && (
                  <span
                    className="vq-pill t-red"
                    title="This network's default security group admits traffic from any address on any port. Every NIC on it inherits that (ADR-017)."
                    style={{ borderColor: "var(--vq-red-line)" }}
                  >
                    allows all ingress
                  </span>
                )}
                {n.legacy_segment && (
                  <span
                    className="vq-pill t-amber"
                    title="Predates the network-kind model: its L2 segment is not guaranteed distinct, so it may share a broadcast domain with another network."
                    style={{ borderColor: "var(--vq-amber-line)" }}
                  >
                    legacy segment
                  </span>
                )}
                <RowMenu inline items={menu} />
              </div>
              {/* Overlays are the thing that spans hosts — cyan earns its place. */}
              <div className={`vq-mono-sm ${n.vni != null ? "t-cyan" : "t-2"}`} style={{ fontSize: 10.5 }}>
                {n.vni != null ? `vni ${n.vni}` : n.vlan != null ? `vlan ${n.vlan}` : "flat"}
              </div>
              <div className="vq-cell vq-mono-sm">
                {n.cidr_v4 ?? <span className="t-4">DHCP</span>}
              </div>
              <div className="vq-cell vq-mono-sm">{n.gateway_v4 ?? <Dash />}</div>
              <div className="vq-cell vq-mono-sm">{pool}</div>
              <div className="vq-cell vq-mono-sm">
                {n.dns.length ? n.dns.join(", ") : <Dash />}
              </div>
              <div className="vq-mono-sm">{nicCounts.get(n.id) ?? 0}</div>
            </TRow>
          );
        })}
      </Table>

      {list.length > 0 && (
        <Grid cols="1fr 1fr" className="vq-split">
          <Card
            title={sg ? `Security group · ${sg.name}` : "Security groups"}
            note={sg ? `attached to ${sgNicCount} NIC${sgNicCount === 1 ? "" : "s"}` : undefined}
          >
            {!sg ? (
              <div style={{ padding: 18 }}>
                <EmptyState
                  headline="No security groups"
                  hint="A NIC with no group is unfiltered."
                />
              </div>
            ) : (
              <Table>
                <THead cols={RULE_COLS}>
                  <div>Dir</div>
                  <div>Proto</div>
                  <div>Ports</div>
                  <div>Remote</div>
                </THead>
                {sg.rules.map((r) => (
                  <TRow key={r.id} cols={RULE_COLS}>
                    <div
                      className="vq-mono-sm"
                      style={{ color: r.direction === "ingress" ? "var(--vq-green)" : "var(--vq-blue)" }}
                    >
                      {r.direction}
                    </div>
                    <div className="vq-mono-sm">{r.protocol}</div>
                    <div className="vq-mono-sm">
                      {r.port_min != null ? (
                        r.port_max != null && r.port_max !== r.port_min ? (
                          `${r.port_min} – ${r.port_max}`
                        ) : (
                          r.port_min
                        )
                      ) : (
                        <Dash />
                      )}
                    </div>
                    <div className="vq-cell vq-mono-sm">{r.remote_cidr ?? "0.0.0.0/0"}</div>
                  </TRow>
                ))}
                {sg.rules.length === 0 && (
                  <div style={{ padding: 18 }}>
                    <EmptyState headline="No rules" hint="All inbound traffic is denied." />
                  </div>
                )}
              </Table>
            )}
          </Card>

          {focus ? (
            <IpAllocations network={focus} />
          ) : (
            <Card padded>
              <EmptyState headline="No IPAM-managed network" hint="Set a CIDR to let the control plane assign addresses." />
            </Card>
          )}
        </Grid>
      )}

      {dialog && <EditDialog edit={dialog.edit} onClose={() => setDialog(null)} />}
    </>
  );
}
