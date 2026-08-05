// Security groups (handoff §10) — master/detail. A group is a small object with
// a list inside it; a table of tables would be worse.

import { useState } from "react";
import Dialog from "@mui/material/Dialog";
import {
  useAddSgRule,
  useCreateSecurityGroup,
  useDeleteSecurityGroup,
  useDeleteSgRule,
  useSecurityGroups,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
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
import type { CreateRuleRequest } from "../api/types";

const COLS = "100px 100px 100px 1fr 1fr 40px";

function CreateGroupDialog({ onClose }: { onClose: () => void }) {
  const create = useCreateSecurityGroup();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>New security group</DialogHead>
      <DialogBody>
        <Field label="Name">
          <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label="Description">
          <Input value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        {create.isError && <ErrorPanel summary="Create rejected" detail={create.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!name || create.isPending}
          onClick={() =>
            create.mutate({ name, description: description || null }, { onSuccess: onClose })
          }
        >
          Create
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function AddRuleDialog({ groupId, onClose }: { groupId: string; onClose: () => void }) {
  const add = useAddSgRule();
  const [direction, setDirection] = useState("ingress");
  const [protocol, setProtocol] = useState("tcp");
  const [ethertype, setEthertype] = useState("IPv4");
  const [portMin, setPortMin] = useState("");
  const [portMax, setPortMax] = useState("");
  const [cidr, setCidr] = useState("");
  const hasPorts = protocol === "tcp" || protocol === "udp";

  const submit = () => {
    const body: CreateRuleRequest = {
      direction,
      ethertype,
      protocol,
      port_min: portMin ? Number(portMin) : null,
      port_max: portMax ? Number(portMax) : portMin ? Number(portMin) : null,
      remote_cidr: cidr.trim() || null,
    };
    add.mutate({ id: groupId, body }, { onSuccess: onClose });
  };

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Add rule</DialogHead>
      <DialogBody>
        <Grid cols="1fr 1fr 1fr">
          <Field label="Direction">
            <Select value={direction} onChange={(e) => setDirection(e.target.value)}>
              <option value="ingress">ingress</option>
              <option value="egress">egress</option>
            </Select>
          </Field>
          <Field label="Ethertype">
            <Select value={ethertype} onChange={(e) => setEthertype(e.target.value)}>
              <option value="IPv4">IPv4</option>
              <option value="IPv6">IPv6</option>
            </Select>
          </Field>
          <Field label="Protocol">
            <Select value={protocol} onChange={(e) => setProtocol(e.target.value)}>
              {["tcp", "udp", "icmp", "any"].map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </Select>
          </Field>
        </Grid>
        <Grid cols="1fr 1fr">
          <Field label="Port min" help={hasPorts ? undefined : "Only tcp and udp carry ports."}>
            <Input
              value={portMin}
              disabled={!hasPorts}
              onChange={(e) => setPortMin(e.target.value)}
            />
          </Field>
          <Field label="Port max">
            <Input
              value={portMax}
              disabled={!hasPorts}
              onChange={(e) => setPortMax(e.target.value)}
            />
          </Field>
        </Grid>
        <Field label="Remote CIDR" help="Blank means any.">
          <Input value={cidr} placeholder="0.0.0.0/0" onChange={(e) => setCidr(e.target.value)} />
        </Field>
        {add.isError && <ErrorPanel summary="Rule rejected" detail={add.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn kind="primary" disabled={add.isPending} onClick={submit}>
          Add rule
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

export function SecurityGroups() {
  const groups = useSecurityGroups();
  const del = useDeleteSecurityGroup();
  const delRule = useDeleteSgRule();
  const { can } = usePermissions();
  const [creating, setCreating] = useState(false);
  const [addingTo, setAddingTo] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const list = groups.data ?? [];
  const selected = list.find((g) => g.id === selectedId) ?? list[0];

  return (
    <>
      <PageHeader
        title="Security groups"
        subtitle={`${list.length} group${
          list.length === 1 ? "" : "s"
        } · rules are enforced per NIC by the host agent`}
        actions={
          can("network:create") && (
            <Btn kind="primary" onClick={() => setCreating(true)}>
              Create group
            </Btn>
          )
        }
      />

      <QueryError error={groups.error} what="security groups" />
      {(del.isError || delRule.isError) && (
        <ErrorPanel summary="Operation failed" detail={del.error || delRule.error} />
      )}

      {groups.isLoading ? (
        <Table>
          <SkeletonRows cols={COLS} />
        </Table>
      ) : list.length === 0 ? (
        <EmptyState
          headline="No security groups yet"
          hint="A NIC with no group is unfiltered — create one to apply stateful filtering."
        />
      ) : (
        <Grid cols="280px 1fr" className="vq-split">
          <Card title="Groups">
            <div style={{ padding: 8 }}>
              {list.map((g) => (
                <button
                  key={g.id}
                  onClick={() => setSelectedId(g.id)}
                  style={{
                    display: "flex",
                    width: "100%",
                    alignItems: "center",
                    justifyContent: "space-between",
                    gap: 8,
                    padding: "8px 10px",
                    borderRadius: "var(--vq-radius-control)",
                    border: 0,
                    cursor: "pointer",
                    fontSize: 12.5,
                    background: g.id === selected?.id ? "var(--vq-blue-soft)" : "transparent",
                    color: g.id === selected?.id ? "var(--vq-blue)" : "var(--vq-text-2)",
                    fontWeight: g.id === selected?.id ? 500 : 400,
                  }}
                >
                  <span>{g.name}</span>
                  <span className="vq-mono-sm">{g.rules.length}</span>
                </button>
              ))}
            </div>
          </Card>

          {selected && (
            <Card
              title={selected.name}
              desc={selected.description}
              actions={
                <div style={{ display: "flex", gap: 8 }}>
                  {can("network:update") && (
                    <Btn onClick={() => setAddingTo(selected.id)}>Add rule</Btn>
                  )}
                  {can("network:delete") && (
                    <Btn kind="destructive" onClick={() => del.mutate(selected.id)}>
                      Delete group
                    </Btn>
                  )}
                </div>
              }
            >
              <Table>
                <THead cols={COLS}>
                  <div>Direction</div>
                  <div>Ethertype</div>
                  <div>Protocol</div>
                  <div>Port range</div>
                  <div>Remote CIDR</div>
                  <div />
                </THead>
                {selected.rules.length === 0 && (
                  <div style={{ padding: 18 }}>
                    <EmptyState
                      headline="No rules"
                      hint="Default-deny ingress, allow egress, stateful. Add a rule to open inbound traffic."
                    />
                  </div>
                )}
                {selected.rules.map((r) => (
                  <TRow key={r.id} cols={COLS}>
                    <div
                      className="vq-mono-sm"
                      style={{
                        color: r.direction === "ingress" ? "var(--vq-green)" : "var(--vq-blue)",
                      }}
                    >
                      {r.direction}
                    </div>
                    <div className="vq-mono-sm">{r.ethertype}</div>
                    <div className="vq-mono-sm">{r.protocol}</div>
                    <div className="vq-mono-sm">
                      {r.port_min != null ? (
                        `${r.port_min} – ${r.port_max ?? r.port_min}`
                      ) : (
                        <Dash />
                      )}
                    </div>
                    <div className="vq-cell vq-mono-sm">{r.remote_cidr ?? "0.0.0.0/0"}</div>
                    <RowMenu
                      items={
                        can("network:update")
                          ? [
                              {
                                label: "Delete rule",
                                danger: true,
                                onClick: () => delRule.mutate({ id: selected.id, ruleId: r.id }),
                              },
                            ]
                          : []
                      }
                    />
                  </TRow>
                ))}
              </Table>
            </Card>
          )}
        </Grid>
      )}

      {creating && <CreateGroupDialog onClose={() => setCreating(false)} />}
      {addingTo && <AddRuleDialog groupId={addingTo} onClose={() => setAddingTo(null)} />}
    </>
  );
}
