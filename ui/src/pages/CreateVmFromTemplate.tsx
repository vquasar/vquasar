// Create VM from template (handoff §7) — the canonical form.
//
// Every override field renders in one of three states: overridden (amber, with
// the template's value beneath it), inherited, or plain default. The submit bar
// shows the exact request that will be sent, containing only the keys that
// actually differ. That is deliberate: it teaches the API and makes the
// override semantics unambiguous.

import { useMemo, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { useCreateVmFromTemplate, useImages, useNetworks, useTemplates } from "../api/hooks";
import { useCrumb } from "../components/Breadcrumb";
import {
  Btn,
  Card,
  EmptyState,
  ErrorPanel,
  Field,
  Grid,
  Input,
  PageHeader,
  QueryError,
  Select,
  SkeletonRows,
  Table,
} from "../ui/kit";
import { formatBytes, formatMib } from "../format";
import type { CreateVmFromTemplateRequest, TemplateOverrides } from "../api/types";

const GIB = 1024 * 1024 * 1024;

// Names become cloud-init hostnames, so they follow hostname rules.
const NAME_RE = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

function nameError(name: string): string | null {
  if (!name) return null;
  if (name.includes("_")) return "Underscores are not permitted.";
  if (/[A-Z]/.test(name)) return "Uppercase is not permitted.";
  if (!NAME_RE.test(name)) return "Use lowercase letters, digits and hyphens.";
  return null;
}

/// An override field: empty means "inherit", a value that differs from the
/// template means "override".
function useOverride(templateValue: string) {
  const [raw, setRaw] = useState("");
  const value = raw === "" ? templateValue : raw;
  const overridden = raw !== "" && raw !== templateValue;
  return { raw, setRaw, value, overridden };
}

export function CreateVmFromTemplate() {
  const { id } = useParams();
  const navigate = useNavigate();
  const templates = useTemplates();
  const images = useImages();
  const networks = useNetworks();
  const launch = useCreateVmFromTemplate();

  const tpl = (templates.data ?? []).find((t) => t.id === id);

  useCrumb(tpl ? `${tpl.name} → new VM` : "Create VM");
  const [name, setName] = useState("");
  const [touched, setTouched] = useState(false);
  const [networkId, setNetworkId] = useState("");
  const [userData, setUserData] = useState("");

  const vcpu = useOverride(tpl ? String(tpl.boot_vcpus) : "");
  const maxVcpu = useOverride(tpl ? String(tpl.max_vcpus) : "");
  const memory = useOverride(tpl ? String(tpl.memory_mib) : "");
  const disk = useOverride(
    tpl?.disk_size_bytes ? String(Math.round(tpl.disk_size_bytes / GIB)) : "",
  );

  const networkOverridden = networkId !== "" && networkId !== (tpl?.network_id ?? "");

  // Only the keys that actually differ travel in the request.
  const overrides = useMemo<TemplateOverrides>(() => {
    const o: TemplateOverrides = {};
    if (!tpl) return o;
    if (vcpu.overridden) o.boot_vcpus = Number(vcpu.value);
    if (maxVcpu.overridden) o.max_vcpus = Number(maxVcpu.value);
    if (memory.overridden) o.memory_mib = Number(memory.value);
    if (disk.overridden && disk.value) o.disk_size_bytes = Math.round(Number(disk.value) * GIB);
    if (networkOverridden) o.network_id = networkId;
    if (userData.trim()) o.cloud_init = { user_data: userData };
    return o;
  }, [tpl, vcpu, maxVcpu, memory, disk, networkOverridden, networkId, userData]);

  const differing = Object.keys(overrides).length;
  const err = nameError(name);

  if (templates.isLoading) {
    return (
      <Table>
        <SkeletonRows cols="1fr 1fr 1fr" />
      </Table>
    );
  }
  if (templates.isError) return <QueryError error={templates.error} what="templates" />;
  if (!tpl) {
    return (
      <EmptyState headline="Template not found" hint="Pick one from the Templates list." />
    );
  }

  const imageName = images.data?.find((i) => i.id === tpl.image_id)?.name ?? tpl.image_id.slice(0, 8);
  const tplNetwork = tpl.network_id
    ? (networks.data?.find((n) => n.id === tpl.network_id)?.name ?? tpl.network_id.slice(0, 8))
    : "none";

  const body: CreateVmFromTemplateRequest = {
    name: name || "<name>",
    template_id: tpl.id,
    ...(differing ? { overrides } : {}),
  };

  const submit = () =>
    launch.mutate(
      { name, template_id: tpl.id, ...(differing ? { overrides } : {}) },
      { onSuccess: () => navigate("/vms") },
    );

  return (
    <div style={{ maxWidth: 1080, display: "flex", flexDirection: "column", gap: 16 }}>
      <PageHeader
        back={
          <span className="vq-backlink">
            <Link to="/templates" className="vq-backlink">
              Templates
            </Link>{" "}
            / {tpl.name}
          </span>
        }
        title="Create VM from template"
        subtitle="Overrides are applied on top of the template and recorded in the VM's spec."
      />

      <Card title="Identity" padded>
        <Grid cols="1fr 1fr" gap={16}>
          <Field
            label="Name"
            state={touched && err ? "invalid" : "default"}
            help={
              touched && err ? err : "Lowercase, hyphenated. Used as the cloud-init hostname."
            }
          >
            <Input
              value={name}
              autoFocus
              state={touched && err ? "invalid" : "default"}
              onChange={(e) => {
                setName(e.target.value);
                setTouched(false);
              }}
              onBlur={() => setTouched(true)}
            />
          </Field>
          <Field
            label="Template"
            help={`${imageName} · ${tplNetwork} · ${tpl.machine_type}`}
          >
            <Select value={tpl.id} onChange={(e) => navigate(`/templates/${e.target.value}/launch`)}>
              {(templates.data ?? []).map((t) => (
                <option key={t.id} value={t.id}>
                  {t.name}
                </option>
              ))}
            </Select>
          </Field>
        </Grid>
      </Card>

      <Card
        title="Overrides"
        note={
          differing ? (
            <span className="t-amber">
              {differing} field{differing === 1 ? "" : "s"} differ from template
            </span>
          ) : (
            "inheriting every field"
          )
        }
        padded
      >
        <Grid cols="repeat(3, 1fr)" gap={16}>
          <Field
            label="Boot vCPU"
            state={vcpu.overridden ? "overridden" : "inherited"}
            help={vcpu.overridden ? `template: ${tpl.boot_vcpus}` : "inherited"}
          >
            <Input
              value={vcpu.value}
              state={vcpu.overridden ? "overridden" : "inherited"}
              onChange={(e) => vcpu.setRaw(e.target.value)}
            />
          </Field>
          <Field
            label="Max vCPU"
            state={maxVcpu.overridden ? "overridden" : "inherited"}
            help={maxVcpu.overridden ? `template: ${tpl.max_vcpus}` : "inherited"}
          >
            <Input
              value={maxVcpu.value}
              state={maxVcpu.overridden ? "overridden" : "inherited"}
              onChange={(e) => maxVcpu.setRaw(e.target.value)}
            />
          </Field>
          <Field
            label="Memory (MiB)"
            state={memory.overridden ? "overridden" : "inherited"}
            help={memory.overridden ? `template: ${tpl.memory_mib}` : "inherited"}
          >
            <Input
              value={memory.value}
              state={memory.overridden ? "overridden" : "inherited"}
              onChange={(e) => memory.setRaw(e.target.value)}
            />
          </Field>
          <Field
            label="Disk size (GiB)"
            state={disk.overridden ? "overridden" : "inherited"}
            help={
              disk.overridden
                ? `template: ${tpl.disk_size_bytes ? formatBytes(tpl.disk_size_bytes) : "image default"}`
                : "inherited"
            }
          >
            <Input
              value={disk.value}
              placeholder="image default"
              state={disk.overridden ? "overridden" : "inherited"}
              onChange={(e) => disk.setRaw(e.target.value)}
            />
          </Field>
          <Field
            label="Network"
            state={networkOverridden ? "overridden" : "inherited"}
            help={networkOverridden ? `template: ${tplNetwork}` : "inherited"}
          >
            <Select
              value={networkId || (tpl.network_id ?? "")}
              state={networkOverridden ? "overridden" : "inherited"}
              onChange={(e) => setNetworkId(e.target.value)}
            >
              <option value="">— none —</option>
              {(networks.data ?? []).map((n) => (
                <option key={n.id} value={n.id}>
                  {n.name}
                </option>
              ))}
            </Select>
          </Field>
          <Field
            label="Machine type"
            help="Fixed by the template — a microVM and a standard guest boot differently."
          >
            <Input value={tpl.machine_type} disabled />
          </Field>
        </Grid>

        <div style={{ marginTop: 16 }}>
          <Field
            label="Cloud-init user-data"
            help="Raw NoCloud user-data, used verbatim. Overrides the template's cloud-init."
            state={userData.trim() ? "overridden" : "default"}
          >
            <textarea
              className={`vq-input${userData.trim() ? " overridden" : ""}`}
              rows={4}
              value={userData}
              placeholder={"#cloud-config\nhostname: my-vm\npackages:\n  - nginx"}
              onChange={(e) => setUserData(e.target.value)}
            />
          </Field>
        </div>
      </Card>

      {launch.isError && <ErrorPanel summary="Create rejected" detail={launch.error} />}

      <div className="vq-submitbar">
        <div className="req">
          POST /api/v1/vms/from-template
          <br />
          {JSON.stringify(body)}
        </div>
        <div style={{ display: "flex", gap: 8, flex: "0 0 auto" }}>
          <Btn tall onClick={() => navigate("/templates")}>
            Cancel
          </Btn>
          <Btn
            kind="primary"
            tall
            disabled={!name || !!err || launch.isPending}
            onClick={submit}
          >
            Create VM
          </Btn>
        </div>
      </div>

      <div className="vq-help">
        Template defaults: {tpl.boot_vcpus} vCPU · {formatMib(tpl.memory_mib)} ·{" "}
        {tpl.disk_size_bytes ? formatBytes(tpl.disk_size_bytes) : "image default"} disk ·{" "}
        {tpl.disk_format}
      </div>
    </div>
  );
}
