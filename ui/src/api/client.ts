// Thin typed fetch client for the /api/v1 surface. Surfaces the control plane's
// {error:{code,message,request_id}} envelope (design section 37) as an Error.

const BASE = "/api/v1";

// The auth layer registers a token getter here so every request carries the
// bearer token without threading it through each call site. Null in dev mode
// (auth disabled) or before login. See src/auth/AuthProvider.tsx.
let tokenGetter: () => string | null = () => null;
export function setTokenGetter(getter: () => string | null) {
  tokenGetter = getter;
}
export function authToken(): string | null {
  return tokenGetter();
}

export interface ApiErrorBody {
  error: { code: string; message: string; request_id: string };
}

export class ApiError extends Error {
  code: string;
  requestId?: string;
  constructor(message: string, code: string, requestId?: string) {
    super(message);
    this.name = "ApiError";
    this.code = code;
    this.requestId = requestId;
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {
    "content-type": "application/json",
    ...(init?.headers as Record<string, string> | undefined),
  };
  const token = tokenGetter();
  if (token) {
    headers.authorization = `Bearer ${token}`;
  }
  const res = await fetch(`${BASE}${path}`, { ...init, headers });
  if (!res.ok) {
    let code = `HTTP_${res.status}`;
    let message = res.statusText;
    let requestId: string | undefined;
    try {
      const body = (await res.json()) as ApiErrorBody;
      if (body?.error) {
        code = body.error.code;
        message = body.error.message;
        requestId = body.error.request_id;
      }
    } catch {
      // non-JSON error body; keep defaults
    }
    throw new ApiError(message, code, requestId);
  }
  if (res.status === 204) {
    return undefined as T;
  }
  return (await res.json()) as T;
}

export const api = {
  get: <T>(path: string) => request<T>(path),
  post: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: "POST", body: body ? JSON.stringify(body) : undefined }),
  put: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: "PUT", body: body ? JSON.stringify(body) : undefined }),
  patch: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: "PATCH", body: body ? JSON.stringify(body) : undefined }),
  del: <T>(path: string) => request<T>(path, { method: "DELETE" }),
};

// Streaming image upload (M14e): sends the raw file as the body with metadata in
// the query string. Not JSON, so it bypasses the typed `api` client.
export async function uploadImage(
  params: Record<string, string>,
  file: File,
): Promise<void> {
  const qs = new URLSearchParams(params).toString();
  const headers: Record<string, string> = {};
  const token = authToken();
  if (token) headers.authorization = `Bearer ${token}`;
  const res = await fetch(`${BASE}/images/upload?${qs}`, {
    method: "POST",
    headers,
    body: file,
  });
  if (!res.ok) {
    let message = res.statusText;
    try {
      const body = (await res.json()) as ApiErrorBody;
      if (body?.error) message = body.error.message;
    } catch {
      /* keep default */
    }
    throw new Error(message);
  }
}
