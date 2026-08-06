// Sign-in gate shown when the control plane enforces authentication and no
// valid session exists (design M12b). Not redesigned in the handoff — restyled
// only as far as the token set takes it.

import { useAuth } from "../auth/AuthProvider";
import { Logo } from "../ui/Mark";
import { Btn, ErrorPanel } from "../ui/kit";

export function Login() {
  const { login, error } = useAuth();
  return (
    <div
      style={{
        minHeight: "100vh",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 24,
        background: "var(--vq-bg)",
      }}
    >
      <div className="vq-card" style={{ width: "100%", maxWidth: 380 }}>
        <div className="vq-card-body" style={{ padding: 28 }}>
          <div
            style={{ display: "flex", alignItems: "center", gap: 10, color: "var(--vq-blue)" }}
          >
            <Logo size={26} />
          </div>
          <div
            style={{
              fontFamily: "var(--vq-font-body)",
              fontSize: 13,
              color: "var(--vq-text-3)",
              margin: "14px 0 22px",
            }}
          >
            Sign in to manage your Cloud Hypervisor fleet.
          </div>
          {error && (
            <div style={{ marginBottom: 16 }}>
              <ErrorPanel summary="Sign-in is not available" detail={error} />
            </div>
          )}
          <Btn kind="primary" tall onClick={login} style={{ width: "100%" }}>
            Sign in
          </Btn>
        </div>
      </div>
    </div>
  );
}
