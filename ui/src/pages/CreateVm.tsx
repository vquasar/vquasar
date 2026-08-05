// Create VM — the full spec form, for when no template fits. Same form
// patterns as /templates/:id/launch (handoff §7): card sections, mono inputs,
// and the exact request shown in the submit bar.

import { useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useCreateVm, useIsos, useNetworks, useSecurityGroups } from "../api/hooks";
import {
  Btn,
  Card,
  Check,
  ErrorPanel,
  Field,
  Grid,
  Input,
  PageHeader,
  Segmented,
  Select,
  Toggle,
} from "../ui/kit";
import { formatBytes } from "../format";
import type { BootSpec, CreateVmRequest, DiskSpec, MachineType } from "../api/types";

const GIB = 1024 * 1024 * 1024;
const WINDOWS_FIRMWARE = "/var/lib/vquasar/shared/firmware/CLOUDHV.fd";
const NAME_RE = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

type BootKind = "direct_kernel" | "firmware";

interface DiskRow {
  path: string;
  readonly: boolean;
}

function nameError(name: string): string | null {
  if (!name) return null;
  if (name.includes("_")) return "Underscores are not permitted.";
  if (/[A-Z]/.test(name)) return "Uppercase is not permitted.";
  if (!NAME_RE.test(name)) return "Use lowercase letters, digits and hyphens.";
  return null;
}

export function CreateVm() {
  const navigate = useNavigate();
  const createVm = useCreateVm();
  const networks = useNetworks();
  const securityGroups = useSecurityGroups();
  const isos = useIsos();

  const [name, setName] = useState("");
  const [touched, setTouched] = useState(false);
  const [machineType, setMachineType] = useState<MachineType>("standard");
  const [vcpus, setVcpus] = useState("2");
  const [memoryMib, setMemoryMib] = useState("2048");
  const [bootKind, setBootKind] = useState<BootKind>("direct_kernel");
  const isMicro = machineType === "microvm";
  const [kernel, setKernel] = useState("/var/lib/vquasar/images/vmlinuz");
  const [initramfs, setInitramfs] = useState("/var/lib/vquasar/images/initrd.img");
  const [cmdline, setCmdline] = useState("root=/dev/vda1 rw console=ttyS0");
  const [firmware, setFirmware] = useState("/var/lib/vquasar/firmware/CLOUDHV.fd");
  const [disks, setDisks] = useState<DiskRow[]>([{ path: "", readonly: false }]);
  const [sysDiskGib, setSysDiskGib] = useState("");
  const [isoSel, setIsoSel] = useState<string[]>([]);
  const [networkId, setNetworkId] = useState("");
  const [sgIds, setSgIds] = useState<string[]>([]);
  const [cloudInit, setCloudInit] = useState("");
  const [powerOn, setPowerOn] = useState(true);

  // One-click Windows-guest scaffold (M15): UEFI firmware boot, a blank virtio
  // system disk, and the virtio-win driver ISO attached read-only. Completing
  // the OS install needs a Windows ISO and (CH being headless) a pre-built
  // virtio image or an unattended serial setup — see docs/windows-guests.md.
  const applyWindowsPreset = () => {
    setMachineType("standard");
    setBootKind("firmware");
    setFirmware(WINDOWS_FIRMWARE);
    setVcpus("2");
    setMemoryMib("4096");
    setSysDiskGib("40");
    setDisks([{ path: "", readonly: false }]);
    setCloudInit("");
    const vw = (isos.data ?? []).find((i) => i.name.toLowerCase().includes("virtio-win"));
    setIsoSel(vw ? [vw.path] : []);
  };

  const setDisk = (i: number, patch: Partial<DiskRow>) =>
    setDisks((d) => d.map((row, idx) => (idx === i ? { ...row, ...patch } : row)));

  const body = useMemo<CreateVmRequest>(() => {
    // A microVM is always direct-kernel; firmware boot is rejected server-side.
    const boot: BootSpec =
      bootKind === "direct_kernel" || isMicro
        ? { type: "direct_kernel", kernel, initramfs: initramfs || null, cmdline: cmdline || null }
        : { type: "firmware", firmware };

    const diskSpecs: DiskSpec[] = [
      // A blank, auto-placed system disk to install onto (the server assigns
      // the path on shared storage from the size alone).
      ...(sysDiskGib
        ? [
            {
              path: "",
              readonly: false,
              image_type: "qcow2" as const,
              size_bytes: Math.round(Number(sysDiskGib) * GIB),
            },
          ]
        : []),
      ...disks
        .filter((d) => d.path.trim() !== "")
        .map((d) => ({ path: d.path.trim(), readonly: d.readonly, image_type: "raw" as const })),
      // ISOs attach read-only as CDs (install media, virtio-win drivers).
      ...isoSel.map((path) => ({ path, readonly: true, image_type: "raw" as const })),
    ];

    return {
      name,
      spec: {
        desired_power_state: powerOn ? "Running" : "Stopped",
        cpu: { boot_vcpus: Number(vcpus) || 1, max_vcpus: Number(vcpus) || 1 },
        memory: { size_mib: Number(memoryMib) || 512 },
        boot,
        disks: diskSpecs,
        network_interfaces: networkId
          ? [{ network_id: networkId, ...(sgIds.length ? { security_groups: sgIds } : {}) }]
          : [],
        placement: {},
        // microVMs forbid the cloud-init seed disk.
        cloud_init: !isMicro && cloudInit.trim() ? { user_data: cloudInit } : null,
        machine_type: machineType,
      },
    };
  }, [
    name,
    powerOn,
    vcpus,
    memoryMib,
    bootKind,
    isMicro,
    kernel,
    initramfs,
    cmdline,
    firmware,
    sysDiskGib,
    disks,
    isoSel,
    networkId,
    sgIds,
    cloudInit,
    machineType,
  ]);

  const err = nameError(name);

  return (
    <div style={{ maxWidth: 1080, display: "flex", flexDirection: "column", gap: 16 }}>
      <PageHeader
        title="Create VM"
        subtitle="Declares desired state. The scheduler places it and the reconcile loop brings it up."
        actions={<Btn onClick={applyWindowsPreset}>Windows guest preset</Btn>}
      />

      <Card title="Identity" padded>
        <Grid cols="1fr 1fr 1fr" gap={16}>
          <Field
            label="Name"
            state={touched && err ? "invalid" : "default"}
            help={touched && err ? err : "Lowercase, hyphenated. Used as the cloud-init hostname."}
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
            label="Machine type"
            help={
              isMicro
                ? "Minimal profile: direct-kernel boot, pvpanic, single PCI segment, no cloud-init seed."
                : "Full device model."
            }
          >
            <Select
              value={machineType}
              onChange={(e) => {
                const mt = e.target.value as MachineType;
                setMachineType(mt);
                if (mt === "microvm") setBootKind("direct_kernel");
              }}
            >
              <option value="standard">standard</option>
              <option value="microvm">microvm</option>
            </Select>
          </Field>
          <Field label="Power on after create">
            <div style={{ height: 32, display: "flex", alignItems: "center" }}>
              <Toggle
                on={powerOn}
                onChange={setPowerOn}
                label={powerOn ? "Yes — desired state Running" : "No — desired state Stopped"}
              />
            </div>
          </Field>
        </Grid>
      </Card>

      <Card title="Compute" padded>
        <Grid cols="repeat(3, 1fr)" gap={16}>
          <Field label="vCPU">
            <Input value={vcpus} onChange={(e) => setVcpus(e.target.value)} />
          </Field>
          <Field label="Memory (MiB)">
            <Input value={memoryMib} onChange={(e) => setMemoryMib(e.target.value)} />
          </Field>
          <Field label="Boot method" help={isMicro ? "microVMs are always direct-kernel." : undefined}>
            <Segmented
              value={isMicro ? "direct_kernel" : bootKind}
              size="tall"
              grow
              onChange={(v) => !isMicro && setBootKind(v)}
              options={[
                { value: "direct_kernel" as BootKind, label: "Direct kernel" },
                { value: "firmware" as BootKind, label: "Firmware" },
              ]}
            />
          </Field>
        </Grid>

        <div style={{ marginTop: 16 }}>
          {bootKind === "direct_kernel" || isMicro ? (
            <Grid cols="1fr 1fr 1fr" gap={16}>
              <Field label="Kernel path">
                <Input value={kernel} onChange={(e) => setKernel(e.target.value)} />
              </Field>
              <Field label="Initramfs path">
                <Input value={initramfs} onChange={(e) => setInitramfs(e.target.value)} />
              </Field>
              <Field label="Kernel cmdline">
                <Input value={cmdline} onChange={(e) => setCmdline(e.target.value)} />
              </Field>
            </Grid>
          ) : (
            <>
              <Field label="Firmware path">
                <Input value={firmware} onChange={(e) => setFirmware(e.target.value)} />
              </Field>
              <div className="vq-warnpanel" style={{ marginTop: 12 }}>
                Cloud Hypervisor is headless and virtio-only, so a fresh Windows install cannot be
                driven interactively. Boot a pre-built image with virtio drivers, or run an
                unattended serial setup — see docs/windows-guests.md.
              </div>
            </>
          )}
        </div>
      </Card>

      <Card title="Storage" padded>
        <Grid cols="1fr 1fr" gap={16}>
          <Field
            label="Blank system disk (GiB)"
            help="Creates a fresh qcow2 on shared storage to install onto; the path is auto-assigned."
          >
            <Input value={sysDiskGib} onChange={(e) => setSysDiskGib(e.target.value)} />
          </Field>
          <Field label="Attach ISOs (read-only)" help="Install media and virtio-win drivers.">
            <div style={{ display: "flex", flexDirection: "column", gap: 7, paddingTop: 6 }}>
              {(isos.data ?? []).map((iso) => (
                <Check
                  key={iso.path}
                  on={isoSel.includes(iso.path)}
                  label={`${iso.name} (${formatBytes(iso.size_bytes)})`}
                  onChange={(on) =>
                    setIsoSel((s) => (on ? [...s, iso.path] : s.filter((p) => p !== iso.path)))
                  }
                />
              ))}
              {(isos.data ?? []).length === 0 && (
                <span className="vq-help">No ISOs in the image store.</span>
              )}
            </div>
          </Field>
        </Grid>

        <div style={{ marginTop: 16, display: "flex", flexDirection: "column", gap: 10 }}>
          {disks.map((d, i) => (
            <div key={i} style={{ display: "flex", gap: 10, alignItems: "flex-end" }}>
              <div style={{ flex: 1 }}>
                <Field label={`Disk ${i + 1} path`}>
                  <Input value={d.path} onChange={(e) => setDisk(i, { path: e.target.value })} />
                </Field>
              </div>
              <div style={{ height: 32, display: "flex", alignItems: "center" }}>
                <Check
                  on={d.readonly}
                  label="read-only"
                  onChange={(on) => setDisk(i, { readonly: on })}
                />
              </div>
              <Btn
                tall
                disabled={disks.length === 1}
                onClick={() => setDisks((ds) => ds.filter((_, idx) => idx !== i))}
              >
                Remove
              </Btn>
            </div>
          ))}
          <div>
            <Btn onClick={() => setDisks((d) => [...d, { path: "", readonly: false }])}>
              Add disk
            </Btn>
          </div>
        </div>
      </Card>

      <Card title="Network" padded>
        <Grid cols="1fr 1fr" gap={16}>
          <Field label="Network">
            <Select value={networkId} onChange={(e) => setNetworkId(e.target.value)}>
              <option value="">— none —</option>
              {(networks.data ?? []).map((n) => (
                <option key={n.id} value={n.id}>
                  {n.name}
                  {n.vlan != null ? ` (vlan ${n.vlan})` : n.vni != null ? ` (vni ${n.vni})` : ""}
                </option>
              ))}
            </Select>
          </Field>
          {networkId && (
            <Field label="Security groups" help="No group leaves the NIC unfiltered.">
              <div style={{ display: "flex", flexDirection: "column", gap: 7, paddingTop: 6 }}>
                {(securityGroups.data ?? []).map((g) => (
                  <Check
                    key={g.id}
                    on={sgIds.includes(g.id)}
                    label={g.name}
                    onChange={(on) =>
                      setSgIds((s) => (on ? [...s, g.id] : s.filter((x) => x !== g.id)))
                    }
                  />
                ))}
              </div>
            </Field>
          )}
        </Grid>
      </Card>

      {!isMicro && (
        <Card title="Cloud-init" padded>
          <Field
            label="User-data"
            help="Raw NoCloud user-data, used verbatim (replaces the generated defaults)."
          >
            <textarea
              className="vq-input"
              rows={5}
              value={cloudInit}
              placeholder={"#cloud-config\nhostname: my-vm\npackages:\n  - nginx"}
              onChange={(e) => setCloudInit(e.target.value)}
            />
          </Field>
        </Card>
      )}

      {createVm.isError && <ErrorPanel summary="Create rejected" detail={createVm.error} />}

      <div className="vq-submitbar">
        <div className="req">
          POST /api/v1/vms
          <br />
          {JSON.stringify(body)}
        </div>
        <div style={{ display: "flex", gap: 8, flex: "0 0 auto" }}>
          <Btn tall onClick={() => navigate("/vms")}>
            Cancel
          </Btn>
          <Btn
            kind="primary"
            tall
            disabled={!name || !!err || createVm.isPending}
            onClick={() => createVm.mutate(body, { onSuccess: () => navigate("/vms") })}
          >
            Create VM
          </Btn>
        </div>
      </div>
    </div>
  );
}
