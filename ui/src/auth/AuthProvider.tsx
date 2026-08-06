// OIDC authentication context (design M12b).
//
// vquasar-control is a resource server: this SPA obtains an access token from the
// external OIDC provider (Authorization Code + PKCE) and sends it as a bearer
// token on every API call. The control plane's public `/auth-config` tells us
// whether auth is enabled and, if so, the issuer + client id to log in with.
// When auth is disabled (dev/lab), we skip login entirely and the control plane
// treats the caller as a superuser.

import {
  createContext,
  ReactNode,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { User, UserManager, WebStorageStateStore } from "oidc-client-ts";
import { authToken, setTokenGetter } from "../api/client";
import type { AuthConfigView } from "../api/types";

interface AuthContextValue {
  /** Still resolving auth-config / processing a redirect. */
  loading: boolean;
  /** Whether the control plane enforces authentication. */
  enabled: boolean;
  /** Signed in (or auth disabled, i.e. nothing to sign into). */
  authenticated: boolean;
  /** OIDC profile of the signed-in user, if any. */
  profile?: User["profile"];
  login: () => void;
  logout: () => void;
  /** Non-fatal error surfaced during discovery or the redirect callback. */
  error?: string;
}

const AuthContext = createContext<AuthContextValue | null>(null);

/// OIDC Authorization Code + PKCE needs `crypto.subtle` to hash the code
/// verifier, and browsers expose it only in a secure context — HTTPS, or
/// localhost. Over plain HTTP to any other host it is simply absent, and the
/// failure surfaces as an opaque "Crypto.subtle is available only in secure
/// contexts" deep inside the OIDC library. Detect it up front and say what is
/// actually wrong.
function insecureContextError(): string | null {
  if (window.isSecureContext) return null;
  return (
    `This page is served over plain HTTP from ${window.location.host}, which the ` +
    `browser treats as an insecure context. Sign-in needs Web Crypto, which is ` +
    `only available over HTTPS (or on localhost). Open the console over HTTPS — ` +
    `the control plane serves it directly — or use a localhost tunnel.`
  );
}

async function fetchAuthConfig(): Promise<AuthConfigView> {
  const res = await fetch("/api/v1/auth-config");
  if (!res.ok) throw new Error(`auth-config: HTTP ${res.status}`);
  return (await res.json()) as AuthConfigView;
}

function buildManager(cfg: AuthConfigView): UserManager {
  const origin = window.location.origin;
  return new UserManager({
    authority: cfg.issuer.replace(/\/$/, ""),
    client_id: cfg.client_id,
    redirect_uri: `${origin}/`,
    post_logout_redirect_uri: `${origin}/`,
    response_type: "code",
    scope: "openid profile email",
    // Silent renew keeps the bearer token fresh without user interaction.
    automaticSilentRenew: true,
    userStore: new WebStorageStateStore({ store: window.localStorage }),
  });
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [loading, setLoading] = useState(true);
  const [enabled, setEnabled] = useState(false);
  const [user, setUser] = useState<User | null>(null);
  const [error, setError] = useState<string | undefined>();
  const [issuer, setIssuer] = useState("");
  const managerRef = useRef<UserManager | null>(null);

  // The token getter reads live state via a ref so client.ts always sees the
  // current token even after a silent renew.
  const userRef = useRef<User | null>(null);
  userRef.current = user;
  useEffect(() => {
    setTokenGetter(() => userRef.current?.access_token ?? null);
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const cfg = await fetchAuthConfig();
        if (cancelled) return;
        if (!cfg.enabled) {
          setEnabled(false);
          setLoading(false);
          return;
        }
        setEnabled(true);
        setIssuer(cfg.issuer);
        const insecure = insecureContextError();
        if (insecure && !cancelled) {
          setError(insecure);
        }
        const mgr = buildManager(cfg);
        managerRef.current = mgr;
        mgr.events.addUserLoaded((u) => setUser(u));
        mgr.events.addUserUnloaded(() => setUser(null));
        mgr.events.addAccessTokenExpired(() => {
          void mgr.signinSilent().catch(() => setUser(null));
        });

        // Prime the provider metadata now so an unreachable or untrusted IdP
        // surfaces on the login screen instead of a silent no-op on click.
        try {
          await mgr.metadataService.getMetadata();
        } catch (e) {
          if (!cancelled) {
            setError(
              `Can't reach the identity provider at ${cfg.issuer}. If it uses an ` +
                `internal CA, open ${cfg.issuer} once in this browser and accept the ` +
                `certificate (or import the CA), then reload. (${e})`,
            );
          }
        }

        // Complete an in-flight redirect (?code=...&state=...).
        const params = new URLSearchParams(window.location.search);
        if (params.has("code") && params.has("state")) {
          try {
            const u = await mgr.signinRedirectCallback();
            if (!cancelled) setUser(u);
          } catch (e) {
            if (!cancelled) setError(String(e));
          }
          // Strip the OAuth params from the address bar.
          window.history.replaceState({}, document.title, window.location.pathname);
        } else {
          const existing = await mgr.getUser();
          if (!cancelled && existing && !existing.expired) setUser(existing);
        }
      } catch (e) {
        if (!cancelled) setError(String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const value = useMemo<AuthContextValue>(() => {
    const authenticated = !enabled || (!!user && !user.expired);
    return {
      loading,
      enabled,
      authenticated,
      profile: user?.profile,
      error,
      login: () => {
        const insecure = insecureContextError();
        if (insecure) {
          setError(insecure);
          return;
        }
        setError(undefined);
        managerRef.current?.signinRedirect().catch((e) =>
          setError(
            `Sign-in couldn't start: ${e}. If the identity provider uses an ` +
              `internal CA, open ${issuer} once in this browser and accept the ` +
              `certificate (or import the CA), then retry.`,
          ),
        );
      },
      logout: () => {
        const mgr = managerRef.current;
        if (mgr) {
          void mgr.signoutRedirect().catch(() => {
            void mgr.removeUser().then(() => setUser(null));
          });
        }
      },
    };
  }, [loading, enabled, user, error, issuer]);

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthContextValue {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error("useAuth must be used within AuthProvider");
  return ctx;
}

/** True once a bearer token is available (or auth is disabled). */
export function hasToken(): boolean {
  return authToken() !== null;
}
