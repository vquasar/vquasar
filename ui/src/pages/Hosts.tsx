// Hosts (handoff §2). Scheduling is deliberately bare mono text rather than a
// chip — a cordoned host already tints its whole row, and two chips per row
// would fight.

import { useState } from "react";
import { Link } from "react-router-dom";
import Dialog from "@mui/material/Dialog";
import {
  useDrainHost,
  useEnrollHost,
  useHosts,
  useRegisterHost,
  useSetHostSchedulable,
} from "../api/hooks";
import { usePermissions } from "../auth/permissions";
import { ACTION } from "../auth/perm";
import {
  Btn,
  Dash,
  DialogBody,
  DialogFoot,
  DialogHead,
  EmptyState,
  ErrorPanel,
  Field,
  Input,
  Pagination,
  PageHeader,
  ProgressCell,
  QueryError,
  RowMenu,
  SkeletonRows,
  StateChip,
  Table,
  THead,
  TRow,
} from "../ui/kit";
import { ageSecs, formatBytes, relTime } from "../format";
import type { DrainResult, EnrollResponse, Host } from "../api/types";

const COLS = "1.3fr 110px 130px 1fr 1.3fr 70px 1fr 110px 40px";
const PAGE_SIZE = 25;

// The control plane marks a host NotReady after this long without a heartbeat;
// the column turns red at the same threshold so the UI never looks calmer than
// the scheduler.
const HEARTBEAT_STALE_SECS = 30;

function RegisterDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [name, setName] = useState("");
  const [endpoint, setEndpoint] = useState("http://127.0.0.1:9500");
  const register = useRegisterHost();

  return (
    <Dialog open={open} onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Register host manually</DialogHead>
      <DialogBody>
        <Field label="Name">
          <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label="Agent gRPC endpoint" help="e.g. http://10.0.0.11:9500">
          <Input value={endpoint} onChange={(e) => setEndpoint(e.target.value)} />
        </Field>
        {register.isError && <ErrorPanel summary="Register failed" detail={register.error} />}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          disabled={!name || !endpoint || register.isPending}
          onClick={() =>
            register.mutate(
              { name, endpoint },
              {
                onSuccess: () => {
                  setName("");
                  onClose();
                },
              },
            )
          }
        >
          Register
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function EnrollDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [name, setName] = useState("");
  const [endpoint, setEndpoint] = useState("http://:9500");
  const enroll = useEnrollHost();
  const [result, setResult] = useState<EnrollResponse | null>(null);

  const close = () => {
    setResult(null);
    setName("");
    enroll.reset();
    onClose();
  };

  const cmd = result
    ? `sudo ./install.sh agent --name ${name} \\\n` +
      `  --bootstrap-token ${result.token} \\\n` +
      `  --bootstrap-url ${result.bootstrap_url ?? "https://<control>:8080/api/v1/enroll/sign"} \\\n` +
      `  --bootstrap-ca /path/to/ca.crt`
    : "";

  return (
    <Dialog open={open} onClose={close} maxWidth="sm" fullWidth>
      <DialogHead>Enroll host</DialogHead>
      <DialogBody>
        {!result ? (
          <>
            <Field label="Name">
              <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
            </Field>
            <Field
              label="Agent gRPC endpoint"
              help="Control dials this; the issued certificate's SAN is derived from it."
            >
              <Input value={endpoint} onChange={(e) => setEndpoint(e.target.value)} />
            </Field>
            {enroll.isError && <ErrorPanel summary="Enroll failed" detail={enroll.error} />}
          </>
        ) : (
          <>
            <div className="vq-warnpanel">
              Copy this now — the join token is shown once and expires in{" "}
              {Math.round(result.expires_in_secs / 60)} minutes.
            </div>
            <div className="vq-inset">{cmd}</div>
            <div className="vq-help">
              Run it on the new host to auto-provision its mTLS certificate. The host turns Ready
              once its agent is up.
            </div>
          </>
        )}
      </DialogBody>
      <DialogFoot>
        <Btn onClick={close}>Close</Btn>
        {!result && (
          <Btn
            kind="primary"
            disabled={!name || !endpoint || enroll.isPending}
            onClick={() => enroll.mutate({ name, endpoint }, { onSuccess: (r) => setResult(r) })}
          >
            Enroll
          </Btn>
        )}
      </DialogFoot>
    </Dialog>
  );
}

function DrainResultDialog({ result, onClose }: { result: DrainResult | null; onClose: () => void }) {
  if (!result) return null;
  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogHead>Drain started</DialogHead>
      <DialogBody>
        <div style={{ fontSize: 12.5 }}>
          Host cordoned. {result.migrating.length} VM
          {result.migrating.length === 1 ? "" : "s"} migrating, {result.skipped.length} left in
          place.
        </div>
        {result.migrating.length > 0 && (
          <div>
            <div className="vq-label">Migrating</div>
            {result.migrating.map((m) => (
              <div key={m.vm_id} className="vq-mono-sm" style={{ color: "var(--vq-cyan)" }}>
                {m.vm_name} → {m.target_host_name}
              </div>
            ))}
          </div>
        )}
        {result.skipped.length > 0 && (
          <div className="vq-warnpanel">
            {result.skipped.map((s) => (
              <div key={s.vm_id}>
                {s.vm_name}: {s.reason}
              </div>
            ))}
          </div>
        )}
      </DialogBody>
      <DialogFoot>
        <Btn kind="primary" onClick={onClose}>
          Close
        </Btn>
      </DialogFoot>
    </Dialog>
  );
}

function schedulingLabel(h: Host, receiving: boolean): { text: string; color: string } {
  if (h.state === "NotReady" || h.state === "Disabled")
    return { text: "Unschedulable", color: "var(--vq-text-4)" };
  if (!h.schedulable) return { text: "Cordoned", color: "var(--vq-amber)" };
  if (receiving) return { text: "Receiving", color: "var(--vq-cyan)" };
  return { text: "Schedulable", color: "var(--vq-text-2)" };
}

export function Hosts() {
  const hosts = useHosts();
  const [registerOpen, setRegisterOpen] = useState(false);
  const [enrollOpen, setEnrollOpen] = useState(false);
  const { can } = usePermissions();
  const setSchedulable = useSetHostSchedulable();
  const drain = useDrainHost();
  const [drainResult, setDrainResult] = useState<DrainResult | null>(null);
  const [page, setPage] = useState(1);
  const manage = can(ACTION.hostCordon);

  const list = hosts.data ?? [];
  // A fleet is hundreds of hosts; render a page of them.
  const pages = Math.max(1, Math.ceil(list.length / PAGE_SIZE));
  const shown = list.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE);
  const ready = list.filter((h) => h.state === "Ready").length;
  const cordoned = list.filter((h) => !h.schedulable).length;
  const chVersions = [
    ...new Set(list.map((h) => h.cloud_hypervisor_version).filter((v): v is string => !!v)),
  ];

  return (
    <>
      <PageHeader
        title="Hosts"
        subtitle={
          list.length
            ? `${ready} Ready · ${cordoned} cordoned · ${
                chVersions.length === 1
                  ? `all agents on cloud-hypervisor ${chVersions[0]}`
                  : `${chVersions.length} cloud-hypervisor versions in the fleet`
              }`
            : "No hosts registered yet."
        }
        actions={
          manage && (
            <>
              <Btn onClick={() => setRegisterOpen(true)}>Register manually</Btn>
              <Btn kind="primary" onClick={() => setEnrollOpen(true)}>
                Enroll host
              </Btn>
            </>
          )
        }
      />

      <QueryError error={hosts.error} what="hosts" />
      {drain.isError && <ErrorPanel summary="Drain failed" detail={drain.error} />}
      {setSchedulable.isError && (
        <ErrorPanel summary="Could not change scheduling" detail={setSchedulable.error} />
      )}

      <Table>
        <THead cols={COLS}>
          <div>Host</div>
          <div>State</div>
          <div>Scheduling</div>
          <div>CPU model</div>
          <div>Memory used</div>
          <div>VMs</div>
          <div>Agent endpoint</div>
          <div>Heartbeat</div>
          <div />
        </THead>

        {hosts.isLoading && <SkeletonRows cols={COLS} />}

        {!hosts.isLoading && list.length === 0 && (
          <div style={{ padding: 18 }}>
            <EmptyState
              headline="No hosts yet"
              hint="Enroll a host to give the control plane somewhere to run VMs."
            />
          </div>
        )}

        {shown.map((h) => {
          const total = h.total_memory_bytes;
          const avail = h.available_memory_bytes;
          const used = total != null && avail != null ? total - avail : null;
          const pct = used != null && total ? (used / total) * 100 : 0;
          const sched = schedulingLabel(h, false);
          const age = ageSecs(h.last_heartbeat);
          const stale = age == null || age > HEARTBEAT_STALE_SECS;

          const menu = manage
            ? [
                h.schedulable
                  ? { label: "Cordon (maintenance)", onClick: () => setSchedulable.mutate({ id: h.id, schedulable: false }) }
                  : { label: "Uncordon", onClick: () => setSchedulable.mutate({ id: h.id, schedulable: true }) },
                {
                  label: "Drain (evacuate VMs)",
                  onClick: () => drain.mutate(h.id, { onSuccess: (r) => setDrainResult(r) }),
                },
              ]
            : [];

          return (
            <TRow key={h.id} cols={COLS} tint={!h.schedulable ? "amber" : undefined}>
              <div className="vq-cell">
                <Link className="vq-name" to={`/hosts/${h.id}`}>
                  {h.name}
                </Link>
              </div>
              <div>
                <StateChip value={h.state} dense />
              </div>
              <div className="vq-cell vq-mono-sm" style={{ color: sched.color }}>
                {sched.text}
              </div>
              <div className="vq-cell vq-mono-sm">
                {h.cpu_model ? (
                  <>
                    {h.cpu_model}
                    {h.logical_cpus != null && ` · ${h.logical_cpus}c`}
                  </>
                ) : (
                  <Dash />
                )}
              </div>
              <div>
                {used != null && total != null ? (
                  <div className="vq-barcell">
                    <ProgressCell
                      pct={pct}
                      width={52}
                      tone={h.schedulable ? "blue" : "amber"}
                      label={`${formatBytes(used)} / ${formatBytes(total)}`}
                    />
                  </div>
                ) : (
                  <Dash />
                )}
              </div>
              <div className="vq-mono-sm">{h.vm_count}</div>
              <div className="vq-cell vq-mono-sm">{h.endpoint}</div>
              <div
                className="vq-cell vq-mono-sm"
                style={stale ? { color: "var(--vq-red)" } : undefined}
              >
                {relTime(h.last_heartbeat) ?? "never"}
              </div>
              <RowMenu items={menu} />
            </TRow>
          );
        })}

        {list.length > 0 && (
          <Pagination
            page={page}
            pages={pages}
            shown={shown.length}
            total={list.length}
            onPage={setPage}
          />
        )}
      </Table>

      <RegisterDialog open={registerOpen} onClose={() => setRegisterOpen(false)} />
      <EnrollDialog open={enrollOpen} onClose={() => setEnrollOpen(false)} />
      <DrainResultDialog result={drainResult} onClose={() => setDrainResult(null)} />
    </>
  );
}
