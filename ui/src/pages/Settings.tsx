// Settings (handoff §14).
//
// Control-plane configuration is now readable over the API (§36), so this shows
// what the cluster is actually set up to do rather than pointing at a file on a
// machine the reader may not have. It still renders no toggles: the values come
// from control.toml and a restart, and a control that silently does nothing is
// worse than a value with its source named.
//
// Protections appear as on/off, never as the values that configure them — the
// endpoint does not carry a key or a connection string, and this page could not
// show one if it wanted to.

import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import { useControlConfig, useHosts } from "../api/hooks";
import { useAuth } from "../auth/AuthProvider";
import { Card, Dash, KV, PageHeader } from "../ui/kit";
import { formatRelative } from "../format";
import { useThemeMode } from "../theme/ThemeMode";
import { Segmented } from "../ui/kit";
import type { AuthConfigView } from "../api/types";

function useAuthConfig() {
  return useQuery({
    queryKey: ["auth-config"],
    queryFn: () => api.get<AuthConfigView>("/auth-config"),
    staleTime: Infinity,
  });
}

export function Settings() {
  const authCfg = useAuthConfig();
  const { enabled } = useAuth();
  const hosts = useHosts();
  const cfg = useControlConfig();
  const { mode, setMode } = useThemeMode();

  const list = hosts.data ?? [];
  const chVersions = [
    ...new Set(list.map((h) => h.cloud_hypervisor_version).filter((v): v is string => !!v)),
  ];
  const kernels = [
    ...new Set(list.map((h) => h.kernel_version).filter((v): v is string => !!v)),
  ];

  return (
    <div style={{ maxWidth: 900, display: "flex", flexDirection: "column", gap: 16 }}>
      <PageHeader
        title="Settings"
        subtitle="Control-plane configuration. Values sourced from control.toml are read-only here."
      />

      <Card title="Control plane">
        <KV k="API origin" v={window.location.origin} labelWidth={200} />
        <KV
          k="Authentication"
          labelWidth={200}
          v={
            enabled ? (
              <span className="t-green">OIDC enforced</span>
            ) : (
              <span className="t-amber">disabled — every caller is a superuser</span>
            )
          }
        />
        <KV k="OIDC issuer" v={authCfg.data?.issuer || <Dash />} labelWidth={200} />
        <KV k="OIDC client id" v={authCfg.data?.client_id || <Dash />} labelWidth={200} />
        <KV k="Version" v={cfg.data?.version ?? <Dash />} labelWidth={200} />
        <KV
          k="Reconcile interval"
          labelWidth={200}
          v={cfg.data ? `${cfg.data.reconcile.interval_secs}s` : <Dash />}
        />
        <KV
          k="Last reconcile pass"
          labelWidth={200}
          v={
            cfg.data?.reconcile.last_pass_at ? (
              formatRelative(cfg.data.reconcile.last_pass_at)
            ) : (
              <span className="t-amber">never — no instance has completed one</span>
            )
          }
        />
        <KV k="Agent transport" v="gRPC over mTLS" labelWidth={200} />
        <KV k="Console poll interval" v="3s" labelWidth={200} />
      </Card>

      <Card title="Protections">
        {[
          ["Authentication", cfg.data?.security.authentication, "every caller is a superuser"],
          ["Encryption at rest", cfg.data?.security.encryption_at_rest, "cloud-init secrets are stored in plaintext"],
          ["Agent mTLS", cfg.data?.security.agent_mtls, "the agent protocol is unauthenticated"],
          ["Database TLS", cfg.data?.security.database_tls, "the database connection is cleartext"],
        ].map(([label, on, warning]) => (
          <KV
            key={String(label)}
            k={String(label)}
            labelWidth={200}
            v={
              on === undefined ? (
                <Dash />
              ) : on ? (
                <span className="t-green">on</span>
              ) : (
                <span className="t-amber">off — {String(warning)}</span>
              )
            }
          />
        ))}
      </Card>

      <Card title="Network policy">
        <KV
          k="NIC policy"
          labelWidth={200}
          v={
            cfg.data ? (
              cfg.data.network.policy_mode === "enforced" ? (
                <span className="t-green">enforced — every NIC gets its network and project defaults</span>
              ) : (
                <span className="t-amber">legacy — a NIC with no groups is unfiltered</span>
              )
            ) : (
              <Dash />
            )
          }
        />
        <KV
          k="Egress"
          labelWidth={200}
          v={
            cfg.data ? (
              cfg.data.network.egress_mode === "enforced" ? (
                <span className="t-green">default-deny</span>
              ) : (
                <span className="t-amber">default-allow — egress rules are refused</span>
              )
            ) : (
              <Dash />
            )
          }
        />
        <KV
          k="Overlay encryption"
          labelWidth={200}
          v={cfg.data?.network.overlay_encryption ?? <Dash />}
        />
        <KV
          k="Physical networks"
          labelWidth={200}
          v={cfg.data?.network.physical_networks.join(", ") || <Dash />}
        />
        <KV k="Permitted VLANs" labelWidth={200} v={cfg.data?.network.provider_vlans || <Dash />} />
      </Card>

      <Card title="Storage">
        <KV
          k="Permitted path roots"
          labelWidth={200}
          v={cfg.data?.storage.allowed_paths.join(", ") ?? <Dash />}
        />
        <KV
          k="Orphaned files"
          labelWidth={200}
          v={
            cfg.data ? (
              cfg.data.storage.orphan_reclaim === "delete" ? (
                "reclaimed automatically"
              ) : cfg.data.storage.orphan_reclaim === "report" ? (
                "reported only — see the events feed"
              ) : (
                <span className="t-amber">not looked for</span>
              )
            ) : (
              <Dash />
            )
          }
        />
      </Card>

      <Card title="Fleet software">
        <KV
          k="Cloud Hypervisor"
          labelWidth={200}
          v={
            chVersions.length === 0 ? (
              <Dash />
            ) : chVersions.length === 1 ? (
              chVersions[0]
            ) : (
              <span className="t-amber">{chVersions.join(", ")} — mixed</span>
            )
          }
        />
        <KV
          k="Host kernels"
          labelWidth={200}
          v={kernels.length ? kernels.join(", ") : <Dash />}
        />
        <KV k="Hosts registered" v={String(list.length)} labelWidth={200} />
      </Card>

      <Card title="Console preferences">
        <div className="vq-setting">
          <div>
            <div className="lbl">Theme</div>
            <div className="exp">
              Stored in this browser. Both themes resolve from the same token set.
            </div>
          </div>
          <Segmented
            value={mode}
            onChange={setMode}
            mono
            options={[
              { value: "dark", label: "DARK" },
              { value: "light", label: "LIGHT" },
            ]}
          />
        </div>
      </Card>

      <Card title="Migration" padded>
        <div style={{ fontFamily: "var(--vq-font-body)", fontSize: 12, color: "var(--vq-text-3)" }}>
          There is no migration policy to show. Cross-CPU-model migration is decided per request —
          the control plane refuses a target whose CPU is missing features the guest could be
          using, and the caller may override that on the request itself. Auto-evacuation does not
          exist: a VM is never moved off an unreachable host without fencing (ADR-014).
        </div>
      </Card>
    </div>
  );
}
