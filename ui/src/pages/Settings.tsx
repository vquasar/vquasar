// Settings (handoff §14) — a new route.
//
// Control-plane configuration lives in control.toml and the API does not expose
// it, so this screen shows what is genuinely observable and says plainly where
// the rest comes from. It does not render toggles that cannot write: a control
// that silently does nothing is worse than a value an operator has to go and
// read.

import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import { useHosts } from "../api/hooks";
import { useAuth } from "../auth/AuthProvider";
import { Card, Dash, KV, PageHeader } from "../ui/kit";
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
        <KV k="Agent transport" v="gRPC over mTLS" labelWidth={200} />
        <KV k="Console poll interval" v="3s" labelWidth={200} />
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

      <Card title="Migration policy" padded>
        <div style={{ fontFamily: "var(--vq-font-body)", fontSize: 12, color: "var(--vq-text-3)" }}>
          Cross-CPU-model migration, auto-evacuation on NotReady and the concurrent-migration limit
          are set in <span className="vq-mono">control.toml</span> and applied at start-up. The API
          does not expose them yet, so they are not editable from the console.
        </div>
      </Card>
    </div>
  );
}
